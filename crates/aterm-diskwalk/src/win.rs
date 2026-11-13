// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOWS CALLS: what the Windows walker is built from. A directory is opened one name at a
//! time relative to its parent's handle and never through a reparse point (`NtCreateFile` with
//! `FILE_OPEN_REPARSE_POINT`), listed a bounded buffer per call from the directory's own index
//! (`FileIdBothDirectoryInfo`: names, sizes, attributes and reparse tags, no file opened), and a
//! regular file's link count and allocation are read by name relative to the directory
//! (`NtQueryInformationByName`, for the files [`LinkReads`] names), which opens nothing either.
//! No call here reads a file's bytes, and a file or folder whose attributes say its data is not
//! on this disk (`OFFLINE`, `RECALL_ON_OPEN`, `RECALL_ON_DATA_ACCESS`) is sized from the listing
//! alone and never entered, so no cloud placeholder is downloaded. A file's allocation is its
//! unnamed stream's: alternate streams are not counted.
//!
//! WHAT THE INDEX KNOWS. NTFS brings a name's index record (its sizes and times) up to date when
//! a handle opened through that name closes, and when the name is read by name; until then the
//! record says what the file was at its last close. Measured on Windows 11 (NTFS): a file created
//! and still open is listed empty however much was written, a file reopened and appended to is
//! listed at its old size, and so is the other name of a hard link. The by-name read is live, and
//! costs about fourteen listed entries (21 µs against 1.5 µs).
//!
//! WHAT AN OPEN DIRECTORY COSTS OTHERS. Windows refuses to rename a directory while a handle to
//! anything below it is open, whatever that handle shares (measured: a handle on `top\mid\leaf`
//! fails a rename of `top\mid` and of `top` with access denied, and of nothing else). A listing's
//! place lives in its handle and no call reopens one at a name, so a [`Lister`] whose handle
//! closes ([`Lister::release`]) lists its directory again on the next one, up to where it was.

use std::ffi::{OsString, c_char, c_void};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};
use std::sync::OnceLock;

use crate::refuse::Refusal;

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SYNCHRONIZE: u32 = 0x0010_0000;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`: the walker denies nobody anything.
const FILE_SHARE_ALL: u32 = 0x0007;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
/// `NtCreateFile` disposition: open what is there, create nothing.
const FILE_OPEN: u32 = 1;
const FILE_DIRECTORY_FILE: u32 = 0x0001;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0020;
const FILE_OPEN_FOR_BACKUP_INTENT: u32 = 0x4000;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const OBJ_CASE_INSENSITIVE: u32 = 0x0040;

pub(crate) const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
/// `OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS`: the data (or the listing) is not on this
/// disk, and reading it would fetch it.
const FILE_ATTRIBUTES_NOT_LOCAL: u32 = 0x0000_1000 | 0x0004_0000 | 0x0040_0000;
/// The reparse tag bit of a name surrogate: a symbolic link, a junction, a volume mount point.
const TAG_NAME_SURROGATE: u32 = 0x2000_0000;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
/// `SYMLINK_FLAG_RELATIVE`: the link's target is relative to the link's directory.
const SYMLINK_FLAG_RELATIVE: u32 = 1;
pub(crate) const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;

/// `FILE_INFO_BY_HANDLE_CLASS::FileAttributeTagInfo`.
const FILE_ATTRIBUTE_TAG_INFO: i32 = 9;
/// `FILE_INFO_BY_HANDLE_CLASS::FileIdBothDirectoryInfo`.
const FILE_ID_BOTH_DIRECTORY_INFO: i32 = 10;
/// `FILE_INFO_BY_HANDLE_CLASS::FileIdBothDirectoryRestartInfo`: the same, from the first entry.
const FILE_ID_BOTH_DIRECTORY_RESTART_INFO: i32 = 11;
/// The file-system flag of a volume whose files can share blocks (ReFS block cloning).
pub(crate) const FILE_SUPPORTS_BLOCK_REFCOUNTING: u32 = 0x0800_0000;
/// How far into its listing a directory can be released: a new handle lists it again to find its
/// place (about 25 ms for this many records), so a directory further in keeps its handle until
/// its listing ends.
const SKIP_MAX: u64 = 16_384;
/// `FILE_INFORMATION_CLASS::FileStatInformation`.
const FILE_STAT_INFORMATION: i32 = 68;

pub(crate) const ERROR_FILE_NOT_FOUND: u32 = 2;
pub(crate) const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_NO_MORE_FILES: u32 = 18;
pub(crate) const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_INVALID_NAME: u32 = 123;
/// What `STATUS_NOT_A_DIRECTORY` reads as.
pub(crate) const ERROR_DIRECTORY: u32 = 267;
const ERROR_NOT_A_REPARSE_POINT: u32 = 4390;
const STATUS_DELETE_PENDING: u32 = 0xC000_0056;

const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;

/// `FILE_ID_BOTH_DIR_INFO`'s fixed part, in bytes; the name follows it.
const DIR_INFO_HEADER: usize = 104;
/// The per-level buffer: 16 KiB, in the `u64` words the kernel's quad-aligned records need.
const LEVEL_WORDS: usize = 16 * 1024 / 8;
/// 1601-01-01 to 1970-01-01, in seconds.
const EPOCH_GAP_S: i64 = 11_644_473_600;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: RawHandle,
    object_name: *const UnicodeString,
    attributes: u32,
    security_descriptor: *const c_void,
    security_quality_of_service: *const c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status: usize,
    information: usize,
}

#[repr(C)]
#[derive(Default)]
struct ByHandleFileInformation {
    attributes: u32,
    creation: [u32; 2],
    access: [u32; 2],
    write: [u32; 2],
    volume_serial: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

#[repr(C)]
#[derive(Default)]
struct AttributeTagInfo {
    attributes: u32,
    reparse_tag: u32,
}

/// `FILE_STAT_INFORMATION` (72 bytes).
#[repr(C)]
#[derive(Default)]
struct FileStatInformation {
    file_id: i64,
    creation: i64,
    access: i64,
    write: i64,
    change: i64,
    allocation: i64,
    end_of_file: i64,
    attributes: u32,
    reparse_tag: u32,
    links: u32,
    effective_access: u32,
}
const _: () = assert!(std::mem::size_of::<FileStatInformation>() == 72);

type FarProc = unsafe extern "system" fn() -> isize;
/// `NtQueryInformationByName` (Windows 10 1709 and later).
type QueryByName = unsafe extern "system" fn(
    *const ObjectAttributes,
    *mut IoStatusBlock,
    *mut c_void,
    u32,
    i32,
) -> i32;

// SAFETY (declaration): kernel32 exports these with these signatures on every supported Windows.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *const c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: RawHandle,
    ) -> RawHandle;
    fn GetFileInformationByHandle(file: RawHandle, info: *mut ByHandleFileInformation) -> i32;
    fn GetFileInformationByHandleEx(
        file: RawHandle,
        class: i32,
        info: *mut c_void,
        size: u32,
    ) -> i32;
    fn GetDriveTypeW(root_path_name: *const u16) -> u32;
    fn GetVolumeInformationByHandleW(
        file: RawHandle,
        volume_name: *mut u16,
        volume_name_size: u32,
        volume_serial: *mut u32,
        maximum_component_length: *mut u32,
        file_system_flags: *mut u32,
        file_system_name: *mut u16,
        file_system_name_size: u32,
    ) -> i32;
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, proc_name: *const c_char) -> Option<FarProc>;
    pub(crate) fn DeviceIoControl(
        device: RawHandle,
        control_code: u32,
        in_buffer: *const c_void,
        in_size: u32,
        out_buffer: *mut c_void,
        out_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
}

// SAFETY (declaration): ntdll exports these with these signatures on every supported Windows.
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtCreateFile(
        file_handle: *mut RawHandle,
        desired_access: u32,
        object_attributes: *const ObjectAttributes,
        io_status_block: *mut IoStatusBlock,
        allocation_size: *const i64,
        file_attributes: u32,
        share_access: u32,
        create_disposition: u32,
        create_options: u32,
        ea_buffer: *const c_void,
        ea_length: u32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

fn last_error() -> u32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .map_or(0, i32::cast_unsigned)
}

/// An `NTSTATUS` as the Win32 code the refusals carry; a name whose delete is pending is gone.
fn status_code(status: i32) -> u32 {
    if status.cast_unsigned() == STATUS_DELETE_PENDING {
        return ERROR_FILE_NOT_FOUND;
    }
    // SAFETY: a pure table lookup on an integer.
    unsafe { RtlNtStatusToDosError(status) }
}

/// A failed call's Win32 code as a refusal.
pub(crate) const fn code_refusal(code: u32) -> Refusal {
    match code {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Refusal::Gone,
        ERROR_DIRECTORY => Refusal::NotDirectory,
        c => Refusal::Unreadable(c.cast_signed()),
    }
}

/// Whether a failed call's code says the name is not there.
pub(crate) const fn is_gone(code: u32) -> bool {
    matches!(code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
}

/// `name` as the one path component a relative open or look takes: not empty, and with nothing
/// that would walk somewhere else (`\`, `/`) or name a stream (`:`).
fn one_component(name: &[u16]) -> bool {
    !name.is_empty()
        && name != [u16::from(b'.')]
        && name != [u16::from(b'.'), u16::from(b'.')]
        && !name
            .iter()
            .any(|&u| u == 0 || b"\\/:".iter().any(|&b| u == u16::from(b)))
}

/// What the drive `letter:` is, before anything on it is opened: never a network, removable or
/// optical drive, whatever the mount table said when the walk was planned.
pub(crate) fn drive_refusal(letter: char) -> Option<Refusal> {
    root_refusal(&format!("{letter}:\\"))
}

/// What the volume whose root is spelled `root` (`C:\`, `\\?\Volume{…}\`) is.
fn root_refusal(root: &str) -> Option<Refusal> {
    let root: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `root` is NUL-terminated and outlives the call, which only reads it and asks the
    // device nothing.
    match unsafe { GetDriveTypeW(root.as_ptr()) } {
        DRIVE_NO_ROOT_DIR => Some(Refusal::Gone),
        DRIVE_REMOTE => Some(Refusal::Remote),
        DRIVE_REMOVABLE | DRIVE_CDROM => Some(Refusal::Removable),
        _ => None,
    }
}

/// The root directory of the drive `letter:`, opened to list.
pub(crate) fn open_drive(letter: char) -> Result<OwnedHandle, Refusal> {
    open_root(&format!("\\\\?\\{letter}:\\"))
}

/// The root directory of the volume `volume` (`Volume{…}`, as the mount manager names it),
/// opened to list: never a network, removable or optical one.
pub(crate) fn open_volume(volume: &str) -> Result<OwnedHandle, Refusal> {
    let root = format!("\\\\?\\{volume}\\");
    match root_refusal(&root) {
        Some(why) => Err(why),
        None => open_root(&root),
    }
}

fn open_root(root: &str) -> Result<OwnedHandle, Refusal> {
    let root: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `root` is NUL-terminated and outlives the call; no security attributes and no
    // template; the flags open a directory (backup semantics) as itself.
    let handle = unsafe {
        CreateFileW(
            root.as_ptr(),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_ALL,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    // `INVALID_HANDLE_VALUE` is all ones.
    if handle.is_null() || handle.addr() == usize::MAX {
        return Err(code_refusal(last_error()));
    }
    // SAFETY: a fresh handle this function owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn object_attributes(parent: RawHandle, name: &UnicodeString) -> ObjectAttributes {
    ObjectAttributes {
        length: u32::try_from(std::mem::size_of::<ObjectAttributes>()).unwrap_or(0),
        root_directory: parent,
        object_name: std::ptr::from_ref(name),
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: std::ptr::null(),
        security_quality_of_service: std::ptr::null(),
    }
}

fn unicode_string(name: &[u16]) -> Result<UnicodeString, u32> {
    if !one_component(name) {
        return Err(ERROR_INVALID_NAME);
    }
    let bytes = u16::try_from(name.len() * 2).map_err(|_| ERROR_INVALID_NAME)?;
    Ok(UnicodeString {
        length: bytes,
        maximum_length: bytes,
        buffer: name.as_ptr(),
    })
}

/// Open `name` inside `parent` as itself: a reparse point is opened, never followed.
fn open_at(
    parent: &OwnedHandle,
    name: &[u16],
    access: u32,
    options: u32,
) -> Result<OwnedHandle, u32> {
    let uname = unicode_string(name)?;
    let attrs = object_attributes(parent.as_raw_handle(), &uname);
    let mut handle: RawHandle = std::ptr::null_mut();
    let mut iosb = IoStatusBlock {
        status: 0,
        information: 0,
    };
    // SAFETY: `attrs` names a live parent handle and a counted UTF-16 name that outlive the call;
    // `handle` and `iosb` are writable; no allocation size and no extended attributes.
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            access | SYNCHRONIZE,
            &raw const attrs,
            &raw mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_ALL,
            FILE_OPEN,
            options
                | FILE_OPEN_REPARSE_POINT
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_FOR_BACKUP_INTENT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(status_code(status));
    }
    // SAFETY: a fresh handle this function owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// The directory `name` inside `parent`, opened to list ([`ERROR_DIRECTORY`] when it is a file).
pub(crate) fn open_dir_at(parent: &OwnedHandle, name: &[u16]) -> Result<OwnedHandle, u32> {
    open_at(
        parent,
        name,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
        FILE_DIRECTORY_FILE,
    )
}

/// What an open directory is.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Info {
    /// Its volume's serial number.
    pub(crate) dev: u64,
    /// Its file id on that volume.
    pub(crate) ino: u64,
    /// A symbolic link, a junction or a volume mount point.
    pub(crate) link: bool,
    /// A cloud placeholder: listing it could fetch it.
    pub(crate) dataless: bool,
}

#[cfg(test)]
std::thread_local! {
    /// The file id this thread reads as a cloud placeholder, which a scratch tree cannot make.
    static FORCED_DATALESS_INO: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// Whether this thread reads as a Windows without `NtQueryInformationByName`.
    static NO_QUERY_BY_NAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The file-system flags this thread reads every volume as having (a ReFS volume is not
    /// something a scratch tree can make).
    static FORCED_VOLUME_FLAGS: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    /// This thread's [`SKIP_MAX`].
    static FORCED_SKIP_MAX: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// The junction target this thread reads as the root of the named volume: a test makes a
    /// junction to a scratch folder and never a link to a whole volume, which a careless
    /// recursive delete of a leftover scratch tree would follow.
    static FORCED_VOLUME_MOUNT: std::cell::RefCell<Option<(String, String)>> =
        const { std::cell::RefCell::new(None) };
}

/// Read a mount point whose reparse target is `target` as the root of the volume `volume` on
/// this thread.
#[cfg(test)]
pub(crate) fn force_volume_mount_for_test(mount: Option<(String, String)>) {
    FORCED_VOLUME_MOUNT.set(mount);
}

/// [`volume_root_name`], for its test.
#[cfg(test)]
pub(crate) fn volume_root_name_for_test(target: &str) -> Option<&str> {
    volume_root_name(target)
}

/// Read every volume as having the file-system flags `flags` on this thread.
#[cfg(test)]
pub(crate) fn force_volume_flags_for_test(flags: Option<u32>) {
    FORCED_VOLUME_FLAGS.set(flags);
}

/// Have a released listing on this thread list at most `max` records again.
#[cfg(test)]
pub(crate) fn force_skip_max_for_test(max: Option<u64>) {
    FORCED_SKIP_MAX.set(max);
}

fn skip_max() -> u64 {
    #[cfg(test)]
    if let Some(max) = FORCED_SKIP_MAX.get() {
        return max;
    }
    SKIP_MAX
}

/// Whether files on `handle`'s volume can share blocks (ReFS block cloning, which a Dev Drive's
/// file copies use): there a file's allocation is not what deleting it frees, and nothing the
/// walker reads says how much of it is shared. A volume whose flags cannot be read shares none.
pub(crate) fn volume_shares_blocks(handle: &OwnedHandle) -> bool {
    #[cfg(test)]
    if let Some(flags) = FORCED_VOLUME_FLAGS.get() {
        return flags & FILE_SUPPORTS_BLOCK_REFCOUNTING != 0;
    }
    let mut flags = 0u32;
    // SAFETY: a live handle; only the flags are asked for, into a writable `u32`; every other
    // output is null with a zero length.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            handle.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut flags,
            std::ptr::null_mut(),
            0,
        )
    };
    ok != 0 && flags & FILE_SUPPORTS_BLOCK_REFCOUNTING != 0
}

/// Read the file or directory with file id `ino` as a cloud placeholder on this thread.
#[cfg(test)]
pub(crate) fn force_dataless_ino_for_test(ino: Option<u64>) {
    FORCED_DATALESS_INO.set(ino);
}

/// Read this thread as a Windows without `NtQueryInformationByName`.
#[cfg(test)]
pub(crate) fn force_no_query_by_name_for_test(off: bool) {
    NO_QUERY_BY_NAME.set(off);
}

fn forced_dataless(ino: u64) -> bool {
    #[cfg(test)]
    {
        FORCED_DATALESS_INO.get() == Some(ino)
    }
    #[cfg(not(test))]
    {
        let _ = ino;
        false
    }
}

/// `handle`'s volume, file id and whether it is a link or a cloud placeholder.
pub(crate) fn info_of(handle: &OwnedHandle) -> Result<Info, u32> {
    let mut info = ByHandleFileInformation::default();
    // SAFETY: a live handle and a writable `BY_HANDLE_FILE_INFORMATION`.
    if unsafe { GetFileInformationByHandle(handle.as_raw_handle(), &raw mut info) } == 0 {
        return Err(last_error());
    }
    let link = if info.attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        false
    } else {
        let mut tag = AttributeTagInfo::default();
        let size = u32::try_from(std::mem::size_of::<AttributeTagInfo>()).unwrap_or(0);
        // SAFETY: a live handle and a writable `FILE_ATTRIBUTE_TAG_INFO` of exactly `size` bytes.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle.as_raw_handle(),
                FILE_ATTRIBUTE_TAG_INFO,
                (&raw mut tag).cast(),
                size,
            )
        };
        // A reparse point whose tag cannot be read is taken for a link: it is never entered.
        ok == 0 || tag.reparse_tag & TAG_NAME_SURROGATE != 0
    };
    let ino = (u64::from(info.index_high) << 32) | u64::from(info.index_low);
    Ok(Info {
        dev: u64::from(info.volume_serial),
        ino,
        link,
        dataless: info.attributes & FILE_ATTRIBUTES_NOT_LOCAL != 0 || forced_dataless(ino),
    })
}

fn query_by_name() -> Option<QueryByName> {
    static FOUND: OnceLock<Option<QueryByName>> = OnceLock::new();
    #[cfg(test)]
    if NO_QUERY_BY_NAME.get() {
        return None;
    }
    *FOUND.get_or_init(|| {
        let module: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
        // SAFETY: `module` is NUL-terminated; ntdll is mapped in every process for its life.
        let ntdll = unsafe { GetModuleHandleW(module.as_ptr()) };
        if ntdll.is_null() {
            return None;
        }
        // SAFETY: a live module handle and a NUL-terminated export name.
        let found = unsafe { GetProcAddress(ntdll, c"NtQueryInformationByName".as_ptr()) }?;
        // SAFETY: the export has `QueryByName`'s signature (`NtQueryInformationByName`), and
        // function pointers of one ABI have one representation.
        Some(unsafe { std::mem::transmute::<FarProc, QueryByName>(found) })
    })
}

/// What a name inside a directory is, read without opening it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Stat {
    pub(crate) attributes: u32,
    pub(crate) reparse_tag: u32,
    pub(crate) ino: u64,
    pub(crate) links: u32,
    pub(crate) alloc: u64,
    pub(crate) logical: u64,
    pub(crate) mtime_s: i64,
}

impl Stat {
    /// What the name is; a link is never followed.
    pub(crate) const fn kind(&self) -> Kind {
        kind_of(self.attributes, self.reparse_tag)
    }

    /// A cloud placeholder: its data is not on this disk.
    pub(crate) fn dataless(&self) -> bool {
        self.attributes & FILE_ATTRIBUTES_NOT_LOCAL != 0 || forced_dataless(self.ino)
    }
}

/// `name` inside `dir`, by one call that opens nothing and follows nothing.
/// # Errors
/// The call's Win32 code; [`ERROR_NOT_SUPPORTED`] on a Windows without the call.
pub(crate) fn stat_at(dir: &OwnedHandle, name: &[u16]) -> Result<Stat, u32> {
    let Some(query) = query_by_name() else {
        return Err(ERROR_NOT_SUPPORTED);
    };
    let uname = unicode_string(name)?;
    let attrs = object_attributes(dir.as_raw_handle(), &uname);
    let mut iosb = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut st = FileStatInformation::default();
    let size = u32::try_from(std::mem::size_of::<FileStatInformation>()).unwrap_or(0);
    // SAFETY: `attrs` names a live directory handle and a counted UTF-16 name that outlive the
    // call; `iosb` and `st` are writable and `st` is exactly `size` bytes.
    let status = unsafe {
        query(
            &raw const attrs,
            &raw mut iosb,
            (&raw mut st).cast(),
            size,
            FILE_STAT_INFORMATION,
        )
    };
    if status < 0 {
        return Err(status_code(status));
    }
    Ok(Stat {
        attributes: st.attributes,
        reparse_tag: st.reparse_tag,
        ino: st.file_id.cast_unsigned(),
        links: st.links,
        alloc: u64::try_from(st.allocation).unwrap_or(0),
        logical: u64::try_from(st.end_of_file).unwrap_or(0),
        mtime_s: unix_seconds(st.write),
    })
}

/// Where the link `name` inside `dir` points, as its reparse data spells it, and whether that is
/// relative to `dir`; `None` when the name is not a symbolic link or a junction.
/// # Errors
/// The open's or the read's Win32 code.
pub(crate) fn link_target_at(
    dir: &OwnedHandle,
    name: &[u16],
) -> Result<Option<(OsString, bool)>, u32> {
    Ok(reparse_at(dir, name)?.map(|(_, target, relative)| (target, relative)))
}

/// The volume mounted at the folder `name` inside `dir`, as the mount manager names it
/// (`Volume{…}`); `None` when the name is anything else, a junction to a directory included.
pub(crate) fn volume_mount_at(dir: &OwnedHandle, name: &[u16]) -> Option<String> {
    match reparse_at(dir, name) {
        Ok(Some((IO_REPARSE_TAG_MOUNT_POINT, target, false))) => {
            let target = target.to_string_lossy();
            #[cfg(test)]
            if let Some(volume) = FORCED_VOLUME_MOUNT
                .with_borrow(|f| f.as_ref().filter(|(at, _)| *at == target).cloned())
            {
                return Some(volume.1);
            }
            volume_root_name(&target).map(str::to_owned)
        }
        _ => None,
    }
}

/// `Volume{…}` when `target` is that volume's root as a mount point's reparse data spells it
/// (`\??\Volume{…}\`), and nothing below the root.
fn volume_root_name(target: &str) -> Option<&str> {
    let name = target.strip_prefix(r"\??\")?.strip_suffix('\\')?;
    let guid = name
        .get(6..)
        .filter(|_| name[..6].eq_ignore_ascii_case("Volume"))?;
    let hex = guid.strip_prefix('{')?.strip_suffix('}')?;
    (hex.len() == 36 && hex.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then_some(name)
}

/// The reparse tag, the target and whether it is relative, of the link `name` inside `dir`.
fn reparse_at(dir: &OwnedHandle, name: &[u16]) -> Result<Option<(u32, OsString, bool)>, u32> {
    let link = open_at(dir, name, FILE_READ_ATTRIBUTES, 0)?;
    let mut buf = vec![0u64; LEVEL_WORDS];
    let mut returned = 0u32;
    // SAFETY: a live handle; no input; `buf` is writable for the byte length passed; a
    // synchronous call (no OVERLAPPED).
    let ok = unsafe {
        DeviceIoControl(
            link.as_raw_handle(),
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            buf.as_mut_ptr().cast(),
            u32::try_from(buf.len() * 8).unwrap_or(0),
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return match last_error() {
            ERROR_NOT_A_REPARSE_POINT => Ok(None),
            code => Err(code),
        };
    }
    let end = usize::try_from(returned).unwrap_or(0);
    // `REPARSE_DATA_BUFFER`: tag, data length, reserved; then the substitute and print names'
    // offsets and lengths, a symbolic link's flags, and the path buffer.
    let (tag, path_at, relative) = match u32_at(&buf, 0) {
        Some(IO_REPARSE_TAG_SYMLINK) => (
            IO_REPARSE_TAG_SYMLINK,
            20,
            u32_at(&buf, 16).is_some_and(|f| f & SYMLINK_FLAG_RELATIVE != 0),
        ),
        Some(IO_REPARSE_TAG_MOUNT_POINT) => (IO_REPARSE_TAG_MOUNT_POINT, 16, false),
        _ => return Ok(None),
    };
    let (Some(at), Some(len)) = (u16_at(&buf, 8), u16_at(&buf, 10)) else {
        return Ok(None);
    };
    let start = path_at + usize::from(at);
    let stop = start + usize::from(len);
    if stop > end {
        return Ok(None);
    }
    let units: Vec<u16> = (start..stop)
        .step_by(2)
        .filter_map(|off| u16_at(&buf, off))
        .collect();
    Ok(Some((tag, OsString::from_wide(&units), relative)))
}

fn byte_at(buf: &[u64], off: usize) -> Option<u8> {
    buf.get(off / 8).map(|w| w.to_le_bytes()[off % 8])
}

fn u16_at(buf: &[u64], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes([
        byte_at(buf, off)?,
        byte_at(buf, off + 1)?,
    ]))
}

fn u32_at(buf: &[u64], off: usize) -> Option<u32> {
    Some(u32::from(u16_at(buf, off)?) | (u32::from(u16_at(buf, off + 2)?) << 16))
}

fn u64_at(buf: &[u64], off: usize) -> Option<u64> {
    Some(u64::from(u32_at(buf, off)?) | (u64::from(u32_at(buf, off + 4)?) << 32))
}

/// A `FILETIME` count (100 ns since 1601) as unix seconds.
const fn unix_seconds(filetime: i64) -> i64 {
    filetime.div_euclid(10_000_000) - EPOCH_GAP_S
}

/// What one entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A directory.
    Dir,
    /// A file.
    File,
    /// A symbolic link, a junction or a volume mount point (counted, never followed).
    Link,
}

const fn kind_of(attributes: u32, reparse_tag: u32) -> Kind {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && reparse_tag & TAG_NAME_SURROGATE != 0 {
        Kind::Link
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Kind::Dir
    } else {
        Kind::File
    }
}

/// One entry as the lister read it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Entry<'a> {
    pub(crate) name: &'a [u16],
    pub(crate) kind: Kind,
    pub(crate) ino: u64,
    pub(crate) nlink: u32,
    pub(crate) alloc: u64,
    pub(crate) logical: u64,
    pub(crate) mtime_s: i64,
    /// A cloud placeholder: its data (a directory's listing) is not on this disk.
    pub(crate) dataless: bool,
    /// A mount of the table the walker must not enter (by name).
    pub(crate) mount_point: bool,
    /// The name vanished between the listing and the read of its link count.
    pub(crate) gone: bool,
}

fn same_name(a: &std::ffi::OsStr, b: &[u16]) -> bool {
    let fold = |u: u16| match u8::try_from(u) {
        Ok(c) => u16::from(c.to_ascii_uppercase()),
        Err(_) => u,
    };
    a.encode_wide().map(fold).eq(b.iter().copied().map(fold))
}

/// Which regular files have their link count, size and allocation read by name
/// (`NtQueryInformationByName`): the directory's index holds no link count, and holds a file's
/// sizes as of its last close (the module's WHAT THE INDEX KNOWS). The read costs about fourteen
/// listed entries (measured: 47,000 reads a second against 680,000 listed entries), so a walk
/// that only sizes folders makes it where the bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkReads {
    /// No file: names and what the listing says of them.
    None,
    /// Files the listing sizes at this many bytes or more, and files it lists as empty (what a
    /// file created and still open is listed as, whatever was written to it). Any other file
    /// counts as one link, at the size the index holds.
    From(u64),
    /// Every file: its link count and its size as they are now.
    Every,
}

/// Where a released listing takes up again on its directory's next handle.
struct Resume {
    /// The last name the closed handle's listing returned.
    last: Vec<u16>,
    /// How many records it had returned, that name's among them.
    returned: u64,
}

/// The offset from the record at `off` to the next one (`0` at the last) and its name's length
/// in bytes.
fn record_header(buf: &[u64], off: usize) -> Option<(usize, usize)> {
    Some((
        usize::try_from(u32_at(buf, off)?).ok()?,
        usize::try_from(u32_at(buf, off + 60)?).ok()?,
    ))
}

/// Whether the record at `off`, whose name is `name_bytes` long, is named `name`.
fn record_named(buf: &[u64], off: usize, name_bytes: usize, name: &[u16]) -> bool {
    name_bytes == name.len() * 2
        && name
            .iter()
            .enumerate()
            .all(|(i, &u)| u16_at(buf, off + DIR_INFO_HEADER + i * 2) == Some(u))
}

/// The by-name reads one call makes before it returns to its caller's clock.
const READS_PER_CALL: u32 = 16;

/// One directory's lister: its handle keeps the listing's place between calls.
pub(crate) struct Lister {
    buf: Vec<u64>,
    skip: Vec<OsString>,
    reads: LinkReads,
    name: Vec<u16>,
    /// Where the next unvisited record of `buf` starts; `None` when `buf` is spent.
    at: Option<usize>,
    /// The records of this directory visited so far, `.` and `..` among them.
    visited: u64,
    /// Whether `buf` holds this directory's records (a spare buffer holds another's).
    filled: bool,
    /// Set while the handle the listing was on is closed ([`Self::release`]).
    resume: Option<Resume>,
}

impl Lister {
    pub(crate) fn new(mut buf: Vec<u64>, skip: Vec<OsString>, reads: LinkReads) -> Self {
        buf.resize(LEVEL_WORDS, 0);
        Self {
            buf,
            skip,
            reads,
            name: Vec::new(),
            at: None,
            visited: 0,
            filled: false,
            resume: None,
        }
    }

    /// Give the level buffer back for the next directory.
    pub(crate) fn into_buffer(self) -> Vec<u64> {
        self.buf
    }

    /// The directory's handle is about to close: remember how far its listing got, so the next
    /// [`Self::next_batch`] takes it up on the directory's next handle (the records already in
    /// the buffer are visited first; they need no listing call). `false`, and nothing
    /// remembered, when the listing is more than [`SKIP_MAX`] records in, which a new handle
    /// would list again: the caller keeps this handle.
    pub(crate) fn release(&mut self) -> bool {
        if self.resume.is_some() || !self.filled {
            return true;
        }
        let mut returned = self.visited;
        let mut last = None;
        let mut next_at = self.at;
        while let Some(off) = next_at {
            let Some((next, name_bytes)) = record_header(&self.buf, off) else {
                break;
            };
            returned += 1;
            last = Some((off, name_bytes));
            next_at = (next != 0).then(|| off + next);
        }
        if returned > skip_max() {
            return false;
        }
        let last = match last {
            // The buffer's last record, not visited yet.
            Some((off, name_bytes)) => (0..name_bytes)
                .step_by(2)
                .filter_map(|i| u16_at(&self.buf, off + DIR_INFO_HEADER + i))
                .collect(),
            // The buffer is spent: the last record visited.
            None => self.name.clone(),
        };
        self.resume = Some(Resume { last, returned });
        true
    }

    /// The name a released listing takes up after.
    #[cfg(test)]
    pub(crate) fn resume_name(&self) -> Option<&[u16]> {
        self.resume.as_ref().map(|r| r.last.as_slice())
    }

    /// One bounded call: at most one listing call and [`READS_PER_CALL`] by-name reads; visit
    /// each entry it got to. `false` when the listing has ended. The first call after a
    /// [`Self::release`] also lists the directory again up to where it was (at most
    /// [`SKIP_MAX`] records).
    /// # Errors
    /// The Win32 code of a listing call that failed.
    pub(crate) fn next_batch(
        &mut self,
        dir: &OwnedHandle,
        visit: &mut dyn FnMut(&Entry<'_>),
    ) -> Result<bool, u32> {
        let mut off = match self.at.take() {
            Some(off) => off,
            None => match self.fill(dir)? {
                Some(off) => off,
                None => return Ok(false),
            },
        };
        let mut reads = 0u32;
        while let Some((next, read)) = self.record(dir, off, visit) {
            if next == 0 {
                break;
            }
            off += next;
            reads += u32::from(read);
            if reads >= READS_PER_CALL {
                self.at = Some(off);
                break;
            }
        }
        Ok(true)
    }

    /// The next buffer of records and where its first unvisited one starts; `None` when the
    /// listing has ended.
    fn fill(&mut self, dir: &OwnedHandle) -> Result<Option<usize>, u32> {
        match self.resume.take() {
            Some(resume) => self.fill_after(dir, &resume),
            None => Ok(self.read(dir, FILE_ID_BOTH_DIRECTORY_INFO)?.then_some(0)),
        }
    }

    /// [`Self::fill`] on `dir`, a new handle to the directory a release closed, whose listing
    /// starts over: list up to the last name the closed handle returned and go on after it. A
    /// name that is not listed any more was deleted since; then as many records as the closed
    /// handle had returned, less that one, are passed over instead (exact unless other names
    /// before it came or went too, which no listing of a changing directory is).
    fn fill_after(&mut self, dir: &OwnedHandle, resume: &Resume) -> Result<Option<usize>, u32> {
        // The name is looked for no further than twice as far in as it was, so the work one call
        // does stays within three times [`SKIP_MAX`] records in a directory that grew meanwhile.
        let mut looked = 0u64;
        let far = resume.returned.saturating_mul(2).saturating_add(64);
        while looked < far && self.read(dir, FILE_ID_BOTH_DIRECTORY_INFO)? {
            let mut off = 0;
            while let Some((next, name_bytes)) = record_header(&self.buf, off) {
                looked += 1;
                if record_named(&self.buf, off, name_bytes, &resume.last) {
                    return if next == 0 {
                        Ok(self.read(dir, FILE_ID_BOTH_DIRECTORY_INFO)?.then_some(0))
                    } else {
                        Ok(Some(off + next))
                    };
                }
                if next == 0 {
                    break;
                }
                off += next;
            }
        }
        let mut left = resume.returned.saturating_sub(1);
        let mut class = FILE_ID_BOTH_DIRECTORY_RESTART_INFO;
        while self.read(dir, class)? {
            class = FILE_ID_BOTH_DIRECTORY_INFO;
            let mut off = 0;
            while let Some((next, _)) = record_header(&self.buf, off) {
                if left == 0 {
                    return Ok(Some(off));
                }
                left -= 1;
                if next == 0 {
                    break;
                }
                off += next;
            }
        }
        Ok(None)
    }

    /// One listing call of `class` into the buffer; `false` when the listing has ended.
    fn read(&mut self, dir: &OwnedHandle, class: i32) -> Result<bool, u32> {
        // SAFETY: `dir` is a live directory handle and `buf` is writable for the byte length
        // passed; the kernel fills at most that many bytes with whole records.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                dir.as_raw_handle(),
                class,
                self.buf.as_mut_ptr().cast(),
                u32::try_from(self.buf.len() * 8).unwrap_or(0),
            )
        };
        if ok != 0 {
            self.filled = true;
            return Ok(true);
        }
        match last_error() {
            // A volume's empty root has not even `.`: its first call finds no file.
            ERROR_NO_MORE_FILES | ERROR_FILE_NOT_FOUND => Ok(false),
            code => Err(code),
        }
    }

    /// Visit the record at `off`: the offset from it to the next one (`0` at the last), and
    /// whether it took a by-name read.
    fn record(
        &mut self,
        dir: &OwnedHandle,
        off: usize,
        visit: &mut dyn FnMut(&Entry<'_>),
    ) -> Option<(usize, bool)> {
        let buf = &self.buf;
        let next = usize::try_from(u32_at(buf, off)?).ok()?;
        let write = u64_at(buf, off + 24)?.cast_signed();
        let end_of_file = u64_at(buf, off + 40)?.cast_signed();
        let allocation = u64_at(buf, off + 48)?.cast_signed();
        let attributes = u32_at(buf, off + 56)?;
        let name_bytes = usize::try_from(u32_at(buf, off + 60)?).ok()?;
        // `EaSize` holds the reparse tag of a reparse point.
        let reparse_tag = u32_at(buf, off + 64)?;
        let ino = u64_at(buf, off + 96)?;
        self.visited += 1;
        self.name.clear();
        for i in (0..name_bytes).step_by(2) {
            self.name.push(u16_at(buf, off + DIR_INFO_HEADER + i)?);
        }
        let name = self.name.as_slice();
        let dot = u16::from(b'.');
        if name.is_empty() || name == [dot] || name == [dot, dot] {
            return Some((next, false));
        }
        let reparse = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
        let mut e = Entry {
            name,
            kind: kind_of(attributes, reparse_tag),
            ino,
            nlink: 1,
            alloc: u64::try_from(allocation).unwrap_or(0),
            logical: u64::try_from(end_of_file).unwrap_or(0),
            mtime_s: unix_seconds(write),
            dataless: attributes & FILE_ATTRIBUTES_NOT_LOCAL != 0 || forced_dataless(ino),
            mount_point: false,
            gone: false,
        };
        let wanted = match self.reads {
            LinkReads::None => false,
            LinkReads::From(floor) => e.alloc >= floor || e.logical >= floor || e.logical == 0,
            LinkReads::Every => true,
        };
        let mut read = false;
        if self.skip.iter().any(|s| same_name(s, name)) {
            e.kind = Kind::Dir;
            e.mount_point = true;
        } else if wanted && e.kind == Kind::File && !reparse && !e.dataless {
            read = true;
            match stat_at(dir, name) {
                Ok(st) if st.kind() == Kind::File => {
                    e.nlink = st.links.max(1);
                    e.alloc = st.alloc;
                    e.logical = st.logical;
                    e.mtime_s = st.mtime_s;
                    e.ino = st.ino;
                }
                Err(code) if is_gone(code) => e.gone = true,
                // What the listing said stands, as one link.
                Ok(_) | Err(_) => {}
            }
        }
        visit(&e);
        Some((next, read))
    }
}
