// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK WATCH'S WINDOWS CALLS, in one auditable file: what std does not say of an open file (the clusters it holds) and does not do to one (mark it with an attribute `CreateFileW` drops; delete it through the very handle that was judged).

use std::io;
use std::os::windows::io::AsRawHandle as _;

/// `FILE_STANDARD_INFO`: the clusters a file holds (`AllocationSize`) beside
/// its length (`EndOfFile`).
#[repr(C)]
#[derive(Default)]
struct FileStandardInfo {
    allocation_size: i64,
    end_of_file: i64,
    number_of_links: u32,
    delete_pending: u8,
    directory: u8,
}

/// `FILE_DISPOSITION_INFO_EX`: the POSIX delete's flags.
#[repr(C)]
struct FileDispositionInfoEx {
    flags: u32,
}

/// `FILE_DISPOSITION_INFO`: the classic delete-on-close mark (`BOOLEAN`).
#[repr(C)]
struct FileDispositionInfo {
    delete_file: u8,
}

/// `FILE_BASIC_INFO`: four `LARGE_INTEGER` times (zero: left as they are)
/// and the attributes, padded to 40 bytes.
#[repr(C)]
#[derive(Default)]
struct FileBasicInfo {
    creation_time: i64,
    last_access_time: i64,
    last_write_time: i64,
    change_time: i64,
    file_attributes: u32,
}

/// `FILE_INFO_BY_HANDLE_CLASS` values.
const FILE_BASIC_INFO: i32 = 0;
const FILE_STANDARD_INFO: i32 = 1;
const FILE_DISPOSITION_INFO: i32 = 4;
const FILE_DISPOSITION_INFO_EX: i32 = 21;

/// `FILE_DISPOSITION_FLAG_*`: delete, unlinking the name at once (POSIX), even
/// a read-only file.
const FILE_DISPOSITION_FLAG_DELETE: u32 = 0x0000_0001;
const FILE_DISPOSITION_FLAG_POSIX_SEMANTICS: u32 = 0x0000_0002;
const FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE: u32 = 0x0000_0010;

/// `DELETE`: the access a handle needs for [`delete_open`].
pub(crate) const DELETE: u32 = 0x0001_0000;
/// `FILE_READ_ATTRIBUTES`: the access [`allocated`] needs.
pub(crate) const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
/// `FILE_FLAG_OPEN_REPARSE_POINT`: open a link (any reparse point) as itself.
pub(crate) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
/// `FILE_ATTRIBUTE_COMPRESSED`: written data is compressed (a folder: what is created in it is).
pub(crate) const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x0000_0800;
/// `FILE_ATTRIBUTE_NOT_CONTENT_INDEXED`: Windows Search leaves it alone.
pub(crate) const FILE_ATTRIBUTE_NOT_CONTENT_INDEXED: u32 = 0x0000_2000;
/// The attributes of a file whose length overstates the clusters it holds: sparse, compressed, offline, or a cloud placeholder recalled on open or on access.
pub(crate) const FILE_ATTRIBUTES_THIN: u32 =
    0x0000_0200 | FILE_ATTRIBUTE_COMPRESSED | 0x0000_1000 | 0x0004_0000 | 0x0040_0000;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFileInformationByHandleEx(
        file: *mut core::ffi::c_void,
        class: i32,
        info: *mut core::ffi::c_void,
        size: u32,
    ) -> i32;
    fn SetFileInformationByHandle(
        file: *mut core::ffi::c_void,
        class: i32,
        info: *const core::ffi::c_void,
        size: u32,
    ) -> i32;
}

/// The bytes of disk the open `file` holds: its allocation, which for a compressed or sparse file is what it really holds, not its length (measured 2026-10-19 on NTFS: 3 MiB of one byte value held 3 MiB plain, 192 KiB in a compressed folder, 0 sparse — the same figures `GetCompressedFileSizeW` reads by path).
/// # Errors
/// The query failed.
pub(crate) fn allocated(file: &std::fs::File) -> io::Result<u64> {
    let mut info = FileStandardInfo::default();
    // SAFETY: the handle is `file`'s own, open for the whole call; `info` is a writable `FILE_STANDARD_INFO` of the size passed, which the call fills.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FILE_STANDARD_INFO,
            (&raw mut info).cast(),
            size_of::<FileStandardInfo>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::try_from(info.allocation_size).unwrap_or(0))
}

/// Add `attributes` to the open `file`'s (opened for writing), its times left as they are: `CreateFileW` takes only a few attributes at creation and drops the rest (`NOT_CONTENT_INDEXED` among them), so they are set here.
/// # Errors
/// The attributes could not be read or set.
pub(crate) fn add_attributes(file: &std::fs::File, attributes: u32) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt as _;
    let info = FileBasicInfo {
        file_attributes: file.metadata()?.file_attributes() | attributes,
        ..FileBasicInfo::default()
    };
    // SAFETY: the handle is `file`'s own, open for the whole call; `info` is a readable `FILE_BASIC_INFO` of the size passed, whose zero times leave the file's own unchanged.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FILE_BASIC_INFO,
            (&raw const info).cast(),
            size_of::<FileBasicInfo>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Delete the file open as `file` (opened with [`DELETE`] access) THROUGH ITS HANDLE: the name goes at once (a POSIX delete; its clusters free when the last handle closes), whatever its read-only bit; where the volume has no POSIX delete, the classic mark, which deletes it when the last handle closes.
/// # Errors
/// Both dispositions were refused.
pub(crate) fn delete_open(file: &std::fs::File) -> io::Result<()> {
    let ex = FileDispositionInfoEx {
        flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: the handle is `file`'s own, open for the whole call; `ex` is a readable `FILE_DISPOSITION_INFO_EX` of the size passed.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FILE_DISPOSITION_INFO_EX,
            (&raw const ex).cast(),
            size_of::<FileDispositionInfoEx>() as u32,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let classic = FileDispositionInfo { delete_file: 1 };
    // SAFETY: as above, with a readable `FILE_DISPOSITION_INFO`.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FILE_DISPOSITION_INFO,
            (&raw const classic).cast(),
            size_of::<FileDispositionInfo>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The longest path the folder readings below take (`UNICODE_STRING`'s).
const MAX_WIDE_PATH: usize = 32_767;

/// `HKEY_LOCAL_MACHINE`: `(LONG)0x80000002` sign-extended to a handle.
const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;

/// `RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND`: a string value,
/// handed back as stored — an expandable one NOT expanded, since
/// `RegGetValueW` would expand it from this process's environment.
const RRF_STRING_AS_STORED: u32 = 0x0000_0002 | 0x0000_0004 | 0x1000_0000;

/// Where the system records its shared program data folder: the
/// `ProgramData` value of this key, `%SystemDrive%\ProgramData` as installed.
const PROFILE_LIST: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";

/// `ERROR_MORE_DATA`: the value is longer than the buffer handed in.
const ERROR_MORE_DATA: i32 = 234;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        key: isize,
        sub_key: *const u16,
        value: *const u16,
        flags: u32,
        kind: *mut u32,
        data: *mut core::ffi::c_void,
        bytes: *mut u32,
    ) -> i32;
}

/// The system's own directory (`%SystemRoot%\System32`) as the SYSTEM says it (`GetSystemDirectoryW`), never as an environment variable a process was handed: what the platform-service rule places an image in (`discover::platform_service`).
pub(crate) fn system_directory() -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    let mut buffer = vec![0u16; 260];
    loop {
        let size = u32::try_from(buffer.len()).ok()?;
        // SAFETY: `buffer` is writable for `size` UTF-16 units for the whole call, which writes at most that many (its terminator included) and answers the units written, or the size it needs when they do not fit, or 0 on failure.
        let n = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), size) } as usize;
        if n == 0 {
            return None;
        }
        if n < buffer.len() {
            buffer.truncate(n);
            return Some(std::ffi::OsString::from_wide(&buffer).into());
        }
        if n > MAX_WIDE_PATH {
            return None;
        }
        buffer.resize(n, 0);
    }
}

/// The machine's shared program data folder (`%ProgramData%`) as the SYSTEM records it, never as an environment variable a process was handed: where Microsoft Defender's platform runs from (`discover::platform_service`).
pub(crate) fn program_data() -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
    let wide = |s: &str| -> Vec<u16> { std::ffi::OsStr::new(s).encode_wide().chain([0]).collect() };
    let (key, value) = (wide(PROFILE_LIST), wide("ProgramData"));
    let mut buffer = vec![0u16; 260];
    let stored = loop {
        let mut bytes = u32::try_from(buffer.len() * 2).ok()?;
        // SAFETY: `key` and `value` are NUL-terminated UTF-16 strings that outlive the call; the predefined key needs no open or close; `buffer` is writable for `bytes` bytes, which the call writes at most (a string value's terminator included) and then sets to the bytes written, or on `ERROR_MORE_DATA` to the bytes it needs.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                RRF_STRING_AS_STORED,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &raw mut bytes,
            )
        };
        match status {
            0 => {
                let units = &buffer[..(bytes as usize / 2).min(buffer.len())];
                let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                break std::ffi::OsString::from_wide(&units[..len]);
            }
            ERROR_MORE_DATA if (bytes as usize) / 2 <= MAX_WIDE_PATH => {
                buffer.resize(bytes as usize / 2 + 1, 0);
            }
            _ => return None,
        }
    };
    expand_system(stored.to_str()?, &system_directory()?)
}

/// `stored` (a folder the registry records, `%SystemDrive%\ProgramData`) with
/// its variables expanded from the system's own directory `system`
/// (`C:\WINDOWS\system32`) alone: `%SystemDrive%` is its drive, `%SystemRoot%`
/// and `%windir%` the folder above it, in any case.
fn expand_system(stored: &str, system: &std::path::Path) -> Option<std::path::PathBuf> {
    let system = system.to_str()?;
    let root = system.rsplit_once('\\')?.0;
    let drive = root.get(..2).filter(|d| d.ends_with(':'))?;
    let mut out = String::new();
    let mut rest = stored;
    while let Some((before, after)) = rest.split_once('%') {
        let (name, next) = after.split_once('%')?;
        let value = if name.eq_ignore_ascii_case("SystemDrive") {
            drive
        } else if name.eq_ignore_ascii_case("SystemRoot") || name.eq_ignore_ascii_case("windir") {
            root
        } else {
            return None;
        };
        out.push_str(before);
        out.push_str(value);
        rest = next;
    }
    out.push_str(rest);
    let path = std::path::PathBuf::from(out);
    path.is_absolute().then_some(path)
}

/// `REPARSE_GUID_DATA_BUFFER`'s header, all a reparse point with no data
/// needs: a tag, the data's length, a reserved word and the owner's `GUID`
/// (24 bytes).
#[cfg(test)]
#[repr(C)]
struct ReparseGuidDataHeader {
    tag: u32,
    data_length: u16,
    reserved: u16,
    guid: [u8; 16],
}

/// `FSCTL_SET_REPARSE_POINT`.
#[cfg(test)]
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;

/// A reparse tag no filter owns, neither Microsoft's (bit 31) nor a name
/// surrogate (bit 29): the kind a cloud placeholder's tag is, which std reads
/// as a plain file or folder, never a link.
#[cfg(test)]
const FOREIGN_REPARSE_TAG: u32 = 0x0000_0077;

#[cfg(test)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn DeviceIoControl(
        device: *mut core::ffi::c_void,
        code: u32,
        in_buffer: *const core::ffi::c_void,
        in_size: u32,
        out_buffer: *mut core::ffi::c_void,
        out_size: u32,
        returned: *mut u32,
        overlapped: *mut core::ffi::c_void,
    ) -> i32;
}

/// TEST FIXTURE: give the open `file` (a file, or an EMPTY folder, opened for writing as itself) a reparse point that is NO LINK ([`FOREIGN_REPARSE_TAG`]) — what a cloud placeholder is to std, made without a privilege (measured 2026-10-19: a plain account sets one; std then reads the file as `is_file`, not `is_symlink`, attributes `0x420`, and an open that does not ask for the point itself fails with `ERROR_CANT_ACCESS_FILE`).
/// # Errors
/// The file system refused the reparse point.
#[cfg(test)]
pub(crate) fn set_foreign_reparse_point(file: &std::fs::File) -> io::Result<()> {
    let header = ReparseGuidDataHeader {
        tag: FOREIGN_REPARSE_TAG,
        data_length: 0,
        reserved: 0,
        guid: [0x11; 16],
    };
    let mut returned: u32 = 0;
    // SAFETY: the handle is `file`'s own, open for the whole call; `header` is a readable `REPARSE_GUID_DATA_BUFFER` header of the size passed (no data follows: its length says 0); no output buffer is passed; the call is synchronous (no `OVERLAPPED`) and writes only `returned`.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            (&raw const header).cast(),
            size_of::<ReparseGuidDataHeader>() as u32,
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each hand-declared structure has the layout the Windows SDK gives it.
    #[test]
    fn the_declared_structures_have_the_sdks_layout() {
        assert_eq!(size_of::<ReparseGuidDataHeader>(), 24);
        assert_eq!(std::mem::offset_of!(ReparseGuidDataHeader, data_length), 4);
        assert_eq!(std::mem::offset_of!(ReparseGuidDataHeader, guid), 8);
        assert_eq!(size_of::<FileStandardInfo>(), 24);
        assert_eq!(std::mem::offset_of!(FileStandardInfo, end_of_file), 8);
        assert_eq!(std::mem::offset_of!(FileStandardInfo, number_of_links), 16);
        assert_eq!(std::mem::offset_of!(FileStandardInfo, delete_pending), 20);
        assert_eq!(std::mem::offset_of!(FileStandardInfo, directory), 21);
        assert_eq!(size_of::<FileBasicInfo>(), 40);
        assert_eq!(std::mem::offset_of!(FileBasicInfo, change_time), 24);
        assert_eq!(std::mem::offset_of!(FileBasicInfo, file_attributes), 32);
        assert_eq!(size_of::<FileDispositionInfoEx>(), 4);
        assert_eq!(size_of::<FileDispositionInfo>(), 1);
    }

    /// The registry's record of a system folder is expanded from the system
    /// directory alone: `%SystemDrive%` is its drive and `%SystemRoot%` /
    /// `%windir%` the folder above it, in any case; a record with no variable
    /// is read as written.
    #[test]
    fn a_stored_folder_is_expanded_from_the_system_directory_alone() {
        let system = std::path::Path::new(r"D:\WINDOWS\system32");
        let expand = |s: &str| expand_system(s, system);
        let at = |s: &str| Some(std::path::PathBuf::from(s));
        assert_eq!(expand(r"%SystemDrive%\ProgramData"), at(r"D:\ProgramData"));
        assert_eq!(expand(r"%systemdrive%\ProgramData"), at(r"D:\ProgramData"));
        assert_eq!(expand(r"%SystemRoot%\Data"), at(r"D:\WINDOWS\Data"));
        assert_eq!(expand(r"%WINDIR%\Data"), at(r"D:\WINDOWS\Data"));
        assert_eq!(
            expand(r"E:\Shared\ProgramData"),
            at(r"E:\Shared\ProgramData")
        );
        assert_eq!(expand(r"%ProgramData%"), None);
        assert_eq!(expand(r"%USERPROFILE%\ProgramData"), None);
        assert_eq!(expand(r"%SystemDrive\ProgramData"), None);
        assert_eq!(expand("ProgramData"), None);
    }

    /// The system's folders are read from the system, not the environment
    /// this process was given: an absolute System32 that holds the system's
    /// own `kernel32.dll`, and an absolute ProgramData folder that exists.
    #[test]
    fn the_system_folders_are_read_from_the_system() {
        let system = system_directory().expect("the system directory");
        assert!(system.is_absolute(), "{system:?}");
        assert!(
            system
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("System32")),
            "{system:?}"
        );
        assert!(system.join("kernel32.dll").is_file(), "{system:?}");
        let data = program_data().expect("ProgramData");
        assert!(data.is_absolute() && data.is_dir(), "{data:?}");
    }

    /// Set in the child [`the_system_folders_ignore_the_environment`] starts:
    /// the file it writes its readings to, instead of running the test.
    const FOLDERS_OUT: &str = "ATERM_DISK_WIN_FOLDERS_OUT";

    /// The system's folders do not follow the environment a process was
    /// handed: a child of this test whose `SystemRoot`, `windir`,
    /// `SystemDrive`, `ProgramData` and `ALLUSERSPROFILE` all name a scratch
    /// folder reads the very folders this process reads.
    #[test]
    fn the_system_folders_ignore_the_environment() {
        let text = |p: Option<std::path::PathBuf>| {
            p.map_or_else(String::new, |p| p.to_string_lossy().into_owned())
        };
        if let Some(out) = std::env::var_os(FOLDERS_OUT) {
            let handed = std::env::var("ProgramData").unwrap_or_default();
            let readings = format!(
                "{}\n{}\n{handed}\n",
                text(system_directory()),
                text(program_data())
            );
            std::fs::write(out, readings).expect("write the readings");
            return;
        }
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let fake = scratch.path().join("NotTheSystem");
        std::fs::create_dir_all(fake.join("System32")).expect("a decoy");
        std::fs::create_dir_all(fake.join("ProgramData")).expect("a decoy");
        let out = scratch.path().join("folders.txt");
        let mut cmd =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
        cmd.args([
            "harness::disk::win::tests::the_system_folders_ignore_the_environment",
            "--exact",
            "--test-threads=1",
        ])
        .env(FOLDERS_OUT, &out);
        for var in [
            "SystemRoot",
            "windir",
            "SystemDrive",
            "ProgramData",
            "ALLUSERSPROFILE",
        ] {
            cmd.env(var, &fake);
        }
        let ran = crate::harness::align::capture_bounded(
            cmd,
            std::time::Duration::from_secs(60),
            64 * 1024,
        )
        .expect("the child runs and is reaped");
        assert!(ran.success(), "{}", String::from_utf8_lossy(&ran.stdout));
        let readings = std::fs::read_to_string(&out).expect("the child's readings");
        let lines: Vec<&str> = readings.lines().collect();
        let fake = fake.to_string_lossy();
        assert_eq!(
            lines,
            [
                text(system_directory()).as_str(),
                text(program_data()).as_str(),
                &*fake,
            ]
        );
    }
}
