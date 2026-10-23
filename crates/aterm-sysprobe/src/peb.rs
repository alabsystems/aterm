// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PEB — another process of this account on Windows, read through a handle.

use crate::win_paths::{self, Plain};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt as _;
use std::os::windows::io::{AsHandle as _, BorrowedHandle};
use std::path::PathBuf;

/// `RTL_USER_PROC_PARAMS_NORMALIZED`: the strings' `Buffer`s are addresses.
const PARAMS_NORMALIZED: u32 = 0x1;

/// The access [`parameters`] and [`crate::process_cwd`] open a process with:
/// `PROCESS_QUERY_LIMITED_INFORMATION` (its image, where its PEB is),
/// `PROCESS_VM_READ` (the PEB) and `SYNCHRONIZE` (whether it exited).
const PEB_READ: u32 = win_paths::PROCESS_QUERY_LIMITED_INFORMATION
    | win_paths::PROCESS_VM_READ
    | win_paths::SYNCHRONIZE;

/// The largest environment block [`parameters`] reads, in bytes.
pub const ENVIRONMENT_CAP: usize = 16 << 20;

/// What one Windows process was started with, as its process environment block holds it now ([`parameters`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessParameters {
    /// The image it runs, as the kernel names the file (`QueryFullProcessImageNameW`: `C:\...\x.exe`).
    pub image: PathBuf,
    /// Its command line (`ProcessParameters.CommandLine`), whole, with no terminating NUL.
    pub command_line: Vec<u16>,
    /// Its environment block (`ProcessParameters.Environment`, its `EnvironmentSize` bytes).
    pub environment: Vec<u16>,
}

/// Read `pid`'s [`ProcessParameters`] from its PEB (including WOW64 32-bit PEB) via [`PEB_READ`].
#[must_use]
pub fn parameters(pid: u32) -> Option<ProcessParameters> {
    let process = win_paths::open_process(pid, PEB_READ)?;
    let h = process.as_handle();
    let image = image_of(h)?;
    let before = params(h)?;
    let command_line = win_paths::read_units(h, before.command_line.0, before.command_line.1)?;
    let size = usize::try_from(before.environment_size)
        .ok()
        .filter(|&n| n > 0 && n <= ENVIRONMENT_CAP)?;
    let environment = win_paths::read_units(h, before.environment, size)?;
    let after = params(h)?;
    (after.started_with() == before.started_with() && !win_paths::exited(h)).then_some(
        ProcessParameters {
            image,
            command_line,
            environment,
        },
    )
}

/// `pid`'s working directory as [`crate::process_cwd`] answers it.
pub(crate) fn cwd(pid: u32) -> Option<PathBuf> {
    let process = win_paths::open_process(pid, PEB_READ)?;
    let h = process.as_handle();
    let cwd = cwd_of(h)?;
    (!win_paths::exited(h)).then(|| without_trailing_separator(cwd))
}

/// Read `CurrentDirectory.DosPath` of process `h` (`PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ`).
#[must_use]
pub fn cwd_of(h: BorrowedHandle<'_>) -> Option<PathBuf> {
    let before = params(h)?;
    let (addr, len) = before.cwd;
    let units = win_paths::read_units(h, addr, len)?;
    let again = win_paths::read_units(h, addr, len)?;
    let after = params(h)?;
    (!units.is_empty() && units == again && after.cwd == before.cwd)
        .then(|| PathBuf::from(OsString::from_wide(&units)))
}

/// Read the executable path of process `h` via `QueryFullProcessImageNameW`.
#[must_use]
pub fn image_of(h: BorrowedHandle<'_>) -> Option<PathBuf> {
    win_paths::image_path(h)
}

/// `cwd` without the one trailing separator a process environment block
/// keeps (`C:\x\` reads `C:\x`), unless what is left would no longer end in
/// a name: a root keeps it (`C:\`, `\\srv\share\`), as `GetCurrentDirectoryW`
/// answers one.
fn without_trailing_separator(cwd: PathBuf) -> PathBuf {
    let Some(s) = cwd.to_str() else {
        return cwd;
    };
    match s.strip_suffix('\\') {
        Some(rest)
            if matches!(
                std::path::Path::new(rest).components().next_back(),
                Some(std::path::Component::Normal(_))
            ) =>
        {
            PathBuf::from(rest)
        }
        _ => cwd,
    }
}

/// The fields of one process's parameters this module reads, from either
/// layout: each string as its address and length in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Params {
    /// `CurrentDirectory.DosPath`.
    cwd: (u64, usize),
    /// `CommandLine`.
    command_line: (u64, usize),
    /// `Environment`, the block's address.
    environment: u64,
    /// `EnvironmentSize`, in bytes.
    environment_size: u64,
    /// `EnvironmentVersion`.
    environment_version: u64,
}

impl Params {
    /// What [`parameters`] reads, which must not move while it reads.
    fn started_with(&self) -> (u64, usize, u64, u64, u64) {
        (
            self.command_line.0,
            self.command_line.1,
            self.environment,
            self.environment_size,
            self.environment_version,
        )
    }
}

/// `h`'s parameters, through its PEB (its `PEB32` under WOW64), read whole;
/// `None` when a read fails, the PEB names none, or they are not normalized
/// or shorter than the structure read.
fn params(h: BorrowedHandle<'_>) -> Option<Params> {
    if let Some(peb32) = win_paths::wow64_peb(h) {
        let head: win_paths::PebHead32 = win_paths::read_plain(h, peb32)?;
        let at = u64::from(head.process_parameters);
        let p = read_whole(h, at, |p: &win_paths::ProcessParameters32| {
            (p.length, p.flags)
        })?;
        Some(Params {
            cwd: string32(p.current_directory),
            command_line: string32(p.command_line),
            environment: u64::from(p.environment),
            environment_size: u64::from(p.environment_size),
            environment_version: u64::from(p.environment_version),
        })
    } else {
        let head: win_paths::PebHead64 = win_paths::read_plain(h, win_paths::basic_peb(h)?)?;
        let at = head.process_parameters;
        let p = read_whole(h, at, |p: &win_paths::ProcessParameters64| {
            (p.length, p.flags)
        })?;
        Some(Params {
            cwd: string64(p.current_directory),
            command_line: string64(p.command_line),
            environment: p.environment,
            environment_size: p.environment_size,
            environment_version: p.environment_version,
        })
    }
}

/// The parameters structure `P` at `at` in `h`, read whole: `None` for a
/// null address, a read that fails, or one whose own `Length` (with its
/// `Flags`, from `length_flags`) says it is shorter than `P` or not yet
/// normalized.
fn read_whole<P: Plain>(
    h: BorrowedHandle<'_>,
    at: u64,
    length_flags: impl Fn(&P) -> (u32, u32),
) -> Option<P> {
    if at == 0 {
        return None;
    }
    let p: P = win_paths::read_plain(h, at)?;
    let (length, flags) = length_flags(&p);
    let whole = usize::try_from(length).is_ok_and(|n| n >= std::mem::size_of::<P>());
    (whole && flags & PARAMS_NORMALIZED != 0).then_some(p)
}

/// A 64-bit process's string as an address and a length in bytes.
fn string64(s: win_paths::RemoteString64) -> (u64, usize) {
    (s.buffer, usize::from(s.length))
}

/// A WOW64 process's string as an address and a length in bytes.
fn string32(s: win_paths::RemoteString32) -> (u64, usize) {
    (u64::from(s.buffer), usize::from(s.length))
}

#[cfg(test)]
mod tests {
    use super::without_trailing_separator;
    use std::path::{Path, PathBuf};

    /// The process environment block's structures, as another process holds
    /// them, have the offsets and sizes of the SDK's symbols (`ntdll`'s
    /// `_PEB`, `_RTL_USER_PROCESS_PARAMETERS` and their WOW64 twins), and
    /// `PROCESS_BASIC_INFORMATION` has the SDK's 64-bit layout.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn the_process_parameters_have_their_sdk_layouts() {
        use crate::win_paths::{
            DriveLetterCurdir32, DriveLetterCurdir64, PebHead32, PebHead64,
            ProcessBasicInformation, ProcessParameters32, ProcessParameters64, RemoteString32,
            RemoteString64,
        };
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<ProcessBasicInformation>(), 48);
        assert_eq!(offset_of!(ProcessBasicInformation, peb_base_address), 8);
        assert_eq!(offset_of!(PebHead64, process_parameters), 0x20);
        assert_eq!(size_of::<PebHead64>(), 0x28);
        assert_eq!(offset_of!(PebHead32, process_parameters), 0x10);
        assert_eq!(size_of::<PebHead32>(), 0x14);
        assert_eq!(size_of::<RemoteString64>(), 16);
        assert_eq!(offset_of!(RemoteString64, buffer), 8);
        assert_eq!(size_of::<RemoteString32>(), 8);
        assert_eq!(offset_of!(RemoteString32, buffer), 4);
        assert_eq!(size_of::<DriveLetterCurdir64>(), 24);
        assert_eq!(size_of::<DriveLetterCurdir32>(), 16);

        assert_eq!(offset_of!(ProcessParameters64, flags), 0x08);
        assert_eq!(offset_of!(ProcessParameters64, standard_input), 0x20);
        assert_eq!(offset_of!(ProcessParameters64, current_directory), 0x38);
        assert_eq!(offset_of!(ProcessParameters64, dll_path), 0x50);
        assert_eq!(offset_of!(ProcessParameters64, image_path_name), 0x60);
        assert_eq!(offset_of!(ProcessParameters64, command_line), 0x70);
        assert_eq!(offset_of!(ProcessParameters64, environment), 0x80);
        assert_eq!(offset_of!(ProcessParameters64, window_title), 0xB0);
        assert_eq!(offset_of!(ProcessParameters64, current_directores), 0xF0);
        assert_eq!(offset_of!(ProcessParameters64, environment_size), 0x3F0);
        assert_eq!(offset_of!(ProcessParameters64, environment_version), 0x3F8);
        assert_eq!(size_of::<ProcessParameters64>(), 0x400);

        assert_eq!(offset_of!(ProcessParameters32, flags), 0x08);
        assert_eq!(offset_of!(ProcessParameters32, current_directory), 0x24);
        assert_eq!(offset_of!(ProcessParameters32, dll_path), 0x30);
        assert_eq!(offset_of!(ProcessParameters32, image_path_name), 0x38);
        assert_eq!(offset_of!(ProcessParameters32, command_line), 0x40);
        assert_eq!(offset_of!(ProcessParameters32, environment), 0x48);
        assert_eq!(offset_of!(ProcessParameters32, window_title), 0x70);
        assert_eq!(offset_of!(ProcessParameters32, current_directores), 0x90);
        assert_eq!(offset_of!(ProcessParameters32, environment_size), 0x290);
        assert_eq!(offset_of!(ProcessParameters32, environment_version), 0x294);
        assert_eq!(size_of::<ProcessParameters32>(), 0x298);
    }

    /// A working directory reads as `GetCurrentDirectoryW` answers it: the
    /// PEB's trailing separator dropped below a root, kept at one.
    #[test]
    fn a_working_directory_drops_its_trailing_separator_but_at_a_root() {
        let read = |s: &str| without_trailing_separator(PathBuf::from(s));
        assert_eq!(read(r"C:\x\"), Path::new(r"C:\x"));
        assert_eq!(read(r"C:\x\y z\").as_os_str(), r"C:\x\y z");
        assert_eq!(read(r"C:\").as_os_str(), r"C:\");
        assert_eq!(read(r"\\srv\share\").as_os_str(), r"\\srv\share\");
        assert_eq!(read(r"\\srv\share\x\").as_os_str(), r"\\srv\share\x");
        assert_eq!(read(r"C:\x").as_os_str(), r"C:\x");
    }

    /// This process's own working directory reads through its environment
    /// block as `std` reads it, in its own spelling (`GetCurrentDirectoryW`'s:
    /// no trailing separator).
    #[test]
    fn a_processs_working_directory_is_read_from_its_environment_block() {
        let own = crate::process_cwd(std::process::id()).expect("our own working directory");
        let want = std::env::current_dir().expect("current dir");
        assert_eq!(own.as_os_str(), want.as_os_str());
        assert_eq!(crate::process_cwd(u32::MAX), None);
        assert_eq!(crate::process_cwd(4), None);
    }
}
