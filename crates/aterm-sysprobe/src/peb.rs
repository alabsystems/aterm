// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PEB reader for processes of the current Windows account.

use crate::win_paths::{self, Plain};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt as _;
use std::os::windows::io::{AsHandle as _, BorrowedHandle};
use std::path::PathBuf;

const PARAMS_NORMALIZED: u32 = 0x1;

const PEB_READ: u32 = win_paths::PROCESS_QUERY_LIMITED_INFORMATION
    | win_paths::PROCESS_VM_READ
    | win_paths::SYNCHRONIZE;

/// The largest environment block [`parameters`] reads, in bytes.
pub const ENVIRONMENT_CAP: usize = 16 << 20;

/// Startup parameters read from a Windows process environment block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessParameters {
    /// Executable image path from `QueryFullProcessImageNameW`.
    pub image: PathBuf,
    /// Command line (`ProcessParameters.CommandLine`) without a terminating NUL.
    pub command_line: Vec<u16>,
    /// Environment block (`ProcessParameters.Environment`).
    pub environment: Vec<u16>,
}

/// Read `pid`'s [`ProcessParameters`] from its PEB (including WOW64 32-bit PEB).
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

/// Read `CurrentDirectory.DosPath` of process `h`.
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

fn string64(s: win_paths::RemoteString64) -> (u64, usize) {
    (s.buffer, usize::from(s.length))
}

fn string32(s: win_paths::RemoteString32) -> (u64, usize) {
    (u64::from(s.buffer), usize::from(s.length))
}

#[cfg(test)]
#[path = "peb_tests.rs"]
mod tests;
