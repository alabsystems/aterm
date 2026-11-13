// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only evidence of Apple's Command Line Tools installer on this Mac
//! ([`super::Activity`]).

#[cfg(any(target_os = "macos", all(test, unix)))]
use std::path::Path;

/// The [`super::Reading`] with the placeholder there or not (`placeholder`), over each running
/// process's executable path (`None` where the kernel names none), read as `aterm init` and the
/// root half read them ([`aterm_types::macos_setup::tools_installer`]): the exact paths of
/// Apple's `softwareupdate` and `installer` ([`is_install_process`]) hold the on-demand
/// placeholder while installing; an absolute path ending in Apple's installer app's own
/// executable identifies its window, whatever its age, with the placeholder it made as it opened
/// beside it or none; a process the kernel names no executable for is neither. So
/// [`super::activity`] reads an install, and Apple's window with or without the placeholder,
/// exactly where [`aterm_types::macos_setup::tools_held_by`] does.
/// Failed process-table or placeholder reads remain unread, distinct from known absence.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn reading_from(
    placeholder: Option<bool>,
    exes: Option<impl IntoIterator<Item = Option<impl AsRef<Path>>>>,
) -> super::Reading {
    use aterm_types::macos_setup::{ToolsInstaller, tools_installer};
    let mut reading = super::Reading {
        placeholder: placeholder.unwrap_or(false),
        unread: placeholder.is_none() || exes.is_none(),
        ..super::Reading::default()
    };
    for exe in exes.into_iter().flatten().flatten() {
        match tools_installer(exe.as_ref(), is_install_process) {
            Some(ToolsInstaller::CommandLine) => reading.install_process = true,
            Some(ToolsInstaller::AppleWindow) => reading.apple_window = true,
            None => {}
        }
    }
    reading
}

/// Whether `path` is one of Apple's command-line installers: compared, never run.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn is_install_process(path: &::std::path::Path) -> bool {
    path == ::std::path::Path::new("/usr/sbin/softwareupdate")
        || path == ::std::path::Path::new("/usr/sbin/installer")
}

/// The pid of the first of `processes` (each a running process's pid and the executable the
/// kernel names for it, `None` where it names none) that is Apple's installer window, read
/// exactly as [`reading_from`] reads the window ([`aterm_types::macos_setup::tools_installer`]):
/// never one of Apple's command-line installers, a path that is not absolute or does not end in
/// the installer app's own executable, or a process with no executable named.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn apple_window_pid(
    processes: impl IntoIterator<Item = (u32, Option<impl AsRef<Path>>)>,
) -> Option<u32> {
    use aterm_types::macos_setup::{ToolsInstaller, tools_installer};
    processes.into_iter().find_map(|(pid, exe)| {
        (tools_installer(exe?.as_ref(), is_install_process) == Some(ToolsInstaller::AppleWindow))
            .then_some(pid)
    })
}

/// The pid of Apple's Command Line Tools installer window open now ([`apple_window_pid`] over
/// every process), which the card's Go to installer brings forward
/// ([`crate::App::go_to_developer_tools_installer`]): one look at the process table, at the
/// press itself, so a window that closed since the page last read this Mac is none.
#[cfg(target_os = "macos")]
pub(super) fn apple_installer_pid() -> Option<u32> {
    apple_window_pid(processes()?)
}

/// This Mac's [`super::Reading`] now: the placeholder there or not as one `lstat` finds it,
/// which follows no link, as `aterm init` reads it, each process's executable as the kernel
/// names it, and the stamp a live run of the root half wrote, matched with its process's start
/// as the kernel recorded it, with that run's parents as the same look at the process table
/// found them ([`run_chain`]).
#[cfg(target_os = "macos")]
pub(super) fn installing_now() -> super::Reading {
    let path = std::path::Path::new(aterm_types::macos_setup::COMMAND_LINE_TOOLS_INSTALLING);
    let placeholder = placeholder_at(path);
    let records = aterm_sysprobe::kproc::records();
    let mut reading = reading_from(
        placeholder,
        records
            .as_deref()
            .map(|records| exes(records).into_iter().map(|(_, exe)| exe)),
    );
    // The one pid the reader asks the kernel about is the stamp's own run.
    let run = std::cell::Cell::new(None);
    reading.live_run = stamp_of(path, 0).is_some_and(|stamp| {
        aterm_types::macos_setup::placeholder_held_by_a_live_run(&stamp, |pid| {
            run.set(Some(pid));
            aterm_sysprobe::kproc::live_start_s(pid)
        })
    });
    if reading.live_run
        && let (Some(pid), Some(records)) = (run.get(), records.as_deref())
    {
        let parents: Vec<(u32, u32)> = records
            .iter()
            .map(|record| (record.pid, record.ppid))
            .collect();
        reading.run_chain = run_chain(pid, &parents);
    }
    reading
}

/// `pid` and its parents, nearest first, by `parents` (each process's pid and its parent's), as
/// many as [`super::RUN_CHAIN`] holds: the chain ends at a process the table does not list, and
/// at launchd (`1`) or the kernel (`0`), which start no tab's program. The places past its end
/// hold `0`.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn run_chain(pid: u32, parents: &[(u32, u32)]) -> [u32; super::RUN_CHAIN] {
    let mut chain = [0; super::RUN_CHAIN];
    let mut at = pid;
    for slot in &mut chain {
        if at <= 1 {
            break;
        }
        *slot = at;
        let Some(&(_, parent)) = parents.iter().find(|(child, _)| *child == at) else {
            break;
        };
        at = parent;
    }
    chain
}

/// Whether the placeholder exists without following links; an unread path is unknown.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn placeholder_at(path: &Path) -> Option<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// The stamp in the placeholder at `path`, as `aterm init` reads it: a regular file `owner`
/// (root, whose root half alone writes a run's stamp) owns and no group or other may write, no
/// longer than a stamp ([`aterm_types::macos_setup::PLACEHOLDER_STAMP_MAX`]), read without
/// following a link or waiting on a pipe; `None` for an empty one (Apple's own) or any other.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn stamp_of(path: &Path, owner: u32) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let max = aterm_types::macos_setup::PLACEHOLDER_STAMP_MAX;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file()
        || meta.uid() != owner
        || meta.mode() & 0o6022 != 0
        || meta.len() == 0
        || meta.len() > max as u64
    {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(max as u64 + 1).read_to_end(&mut bytes).ok()?;
    (!bytes.is_empty() && bytes.len() <= max).then_some(bytes)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn installing_now() -> super::Reading {
    super::Reading::default()
}

/// Every process running now, by its pid, with the executable the kernel names for it
/// ([`exes`]).
#[cfg(target_os = "macos")]
fn processes() -> Option<Vec<(u32, Option<std::path::PathBuf>)>> {
    Some(exes(&aterm_sysprobe::kproc::records()?))
}

/// Each of `records` that runs, by its pid, with the executable the kernel names for it (`None`
/// where it names none); one that exited before its executable was read is left out.
#[cfg(target_os = "macos")]
fn exes(records: &[aterm_sysprobe::kproc::Record]) -> Vec<(u32, Option<std::path::PathBuf>)> {
    records
        .iter()
        .filter(|record| record.pid > 0 && !record.zombie)
        .filter_map(|record| match aterm_uds::holders::exe_of(record.pid) {
            aterm_uds::holders::Exe::At(path) => Some((record.pid, Some(path))),
            aterm_uds::holders::Exe::Unnamed => Some((record.pid, None)),
            aterm_uds::holders::Exe::Exited => None,
        })
        .collect()
}

#[cfg(all(test, unix))]
#[path = "probe_tests.rs"]
mod tests;
