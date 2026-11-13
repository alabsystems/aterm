// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux's process listing and descriptor tables (`<proc>/<pid>/fd`), answering
//! [`super::listed_pids`], [`super::euid_of`], [`super::scan_file_holders`],
//! [`super::scan_listener_holders`] and [`super::open_files`] from `/proc` in production or a
//! scratch tree in tests.
//!
//! Two kinds of same-uid process have root-owned tables (`task_dump_owner` in `fs/proc/base.c`):
//! zombies holding nothing ([`Table::Exited`]), and non-dumpable live processes such as
//! `ssh-agent` ([`Table::Sealed`]); any other refusal is [`Table::Unreadable`], which a scan
//! counts clear once the process has exited and outside it once the process runs as another
//! user (`read_of`).

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use aterm_sysprobe::linux::UnixDiagListener;

use super::HolderScan;

/// The kernel's procfs.
#[cfg(target_os = "linux")]
pub(super) const PROC: &str = "/proc";

/// What `<proc>/<pid>` says of the process's descriptor table.
#[derive(Debug)]
pub(super) enum Table {
    /// The table's entries.
    Listed(std::fs::ReadDir),
    /// It has exited, or is a zombie whose every thread has: it holds nothing.
    Exited,
    /// Alive, and its table kept from every reader but root (a non-dumpable process).
    Sealed,
    /// The table could not be read for any other reason.
    Unreadable(io::Error),
}

/// What `<proc>/<pid>/stat` and `status` establish about a process's life.
#[derive(Debug, PartialEq, Eq)]
enum ProcessState {
    /// No `stat`: the process has been reaped since it was listed.
    Reaped,
    /// A zombie (`Z`) or dead (`X`/`x`) task that is its process's only thread.
    Zombie,
    /// A zombie thread-group leader whose other threads still run and share its table.
    LeaderOnly,
    /// A task in a known live state.
    Live,
}

/// The process's state (the letter after `stat`'s closing parenthesis) and, for a zombie or
/// dead task, its thread count in `status`; `None` when either required read is unknown. Both
/// are read lossily ([`crate::process::read_proc_text`]): their command name is raw bytes.
fn process_state(dir: &Path) -> Option<ProcessState> {
    let read = |name: &str| crate::process::read_proc_text(&dir.join(name));
    state_of(read("stat"), || read("status").ok())
}

/// [`process_state`]'s judgment of its `stat` read and, for a zombie or dead task only, its
/// `status`. A process gone by the `stat` read is reaped by the rule every `/proc` reader here
/// judges a gone one by ([`crate::process::stat_read`]: `ENOENT` or `ESRCH`).
fn state_of(
    stat: io::Result<String>,
    status: impl FnOnce() -> Option<String>,
) -> Option<ProcessState> {
    use crate::process::procfs::StatRead;
    let stat = match crate::process::stat_read(stat) {
        StatRead::Text(stat) => stat,
        StatRead::Gone => return Some(ProcessState::Reaped),
        StatRead::Unreadable => return None,
    };
    let state = stat.rsplit_once(')')?.1.trim_start().chars().next()?;
    match state {
        'Z' | 'X' | 'x' => {}
        'R' | 'S' | 'D' | 'T' | 't' | 'W' | 'K' | 'P' | 'I' => return Some(ProcessState::Live),
        _ => return None,
    }
    let threads = status().and_then(|status| {
        status
            .lines()
            .find_map(|l| l.strip_prefix("Threads:"))
            .map(str::trim)
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<u32>().ok())
    });
    match threads {
        Some(1) => Some(ProcessState::Zombie),
        Some(n) if n > 1 => Some(ProcessState::LeaderOnly),
        _ => None,
    }
}

/// `<proc>/<pid>`'s descriptor table.
pub(super) fn table(proc: &Path, pid: u32) -> Table {
    let dir = proc.join(pid.to_string());
    let refused = match std::fs::read_dir(dir.join("fd")) {
        Ok(fds) => return Table::Listed(fds),
        Err(e) => e,
    };
    match process_state(&dir) {
        Some(ProcessState::Reaped | ProcessState::Zombie) => return Table::Exited,
        Some(ProcessState::LeaderOnly) | None => return Table::Unreadable(refused),
        Some(ProcessState::Live) => {}
    }
    let owner = |p: &Path| std::fs::metadata(p).ok().map(|m| m.uid());
    match (owner(&dir.join("fd")), owner(&dir)) {
        (Some(table), Some(process))
            if refused.kind() == io::ErrorKind::PermissionDenied && table != process =>
        {
            Table::Sealed
        }
        _ => Table::Unreadable(refused),
    }
}

/// The pids a `/proc` listing's entry names: each positive number in decimal digits alone, any
/// other name (`self`, `+101`) skipped. An entry that does not read fails the listing, since a
/// listing cut short proves no absence.
///
/// # Errors
///
/// The first entry that did not read.
pub(super) fn pids_listed(
    entries: impl IntoIterator<Item = io::Result<OsString>>,
) -> io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    for entry in entries {
        if let Some(pid) = entry?
            .to_str()
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<u32>().ok())
            && pid > 0
        {
            pids.push(pid);
        }
    }
    Ok(pids)
}

/// Every pid under `proc` ([`pids_listed`]).
///
/// # Errors
///
/// `proc` could not be listed, or one of its entries did not read.
pub(super) fn listed_pids_in(proc: &Path) -> io::Result<Vec<u32>> {
    pids_listed(std::fs::read_dir(proc)?.map(|e| e.map(|e| e.file_name())))
}

/// The effective uid `pid` runs as: its directory's owner. The kernel keeps `/proc/<pid>` the
/// effective uid's when the process is non-dumpable and gives root only the entries inside
/// (`task_dump_owner` spares a `0555` directory, as `docs/measured/linux-lane-2026-10-23.md`
/// records).
///
/// # Errors
///
/// `NotFound` once `pid` is gone; the `stat`'s error otherwise.
pub(super) fn euid_in(proc: &Path, pid: u32) -> io::Result<u32> {
    std::fs::metadata(proc.join(pid.to_string())).map(|m| m.uid())
}

/// Every pid under `proc` that runs as effective uid `me` ([`euid_in`]): a pid gone between the
/// listing and its `stat` is not `me`'s.
///
/// # Errors
///
/// `proc` could not be listed whole ([`listed_pids_in`]).
pub(super) fn uid_pids_in(proc: &Path, me: u32) -> io::Result<Vec<u32>> {
    Ok(listed_pids_in(proc)?
        .into_iter()
        .filter(|pid| euid_in(proc, *pid).ok() == Some(me))
        .collect())
}

/// What one table says of the file whose device and inode are `key`.
#[derive(Debug, PartialEq, Eq)]
enum Read {
    Holds,
    Clear,
    Sealed,
    /// Its process runs as another effective user now, outside a scan of `me`'s processes
    /// ([`read_of`]).
    Left,
    Unreadable,
}

/// Whether `<proc>/<pid>` names another effective user than `me` now ([`euid_in`]). `false`
/// once the directory is gone.
pub(super) fn runs_as_another_user_in(proc: &Path, pid: u32, me: u32) -> bool {
    euid_in(proc, pid).is_ok_and(|uid| uid != me)
}

/// What `read` makes of `pid`'s table, an unknown answer read [`Read::Clear`] once `pid` has
/// exited since (reaped, or a zombie whose every thread has: [`table`]'s [`Table::Exited`]),
/// and [`Read::Left`] once it runs as another effective user than `me`
/// ([`runs_as_another_user_in`]). A process that exits while its listed table is read holds
/// nothing, yet the kernel makes its `fd` directory root's once it has no memory map
/// (`task_dump_owner`), so each entry read after that fails `EACCES`, not `ENOENT`. A scan lists
/// only `me`'s processes, so a process that execs a set-user-ID program (`sudo`, `su`,
/// `passwd`) between the listing and its read is outside it, as it would be listed a moment
/// later: the exec makes its directory and its table root's (`task_dump_owner`, the process now
/// non-dumpable), so its table is refused and would otherwise leave the whole scan unknown.
fn read_of(proc: &Path, pid: u32, me: u32, read: impl FnOnce(Table) -> Read) -> Read {
    match read(table(proc, pid)) {
        Read::Unreadable
            if matches!(
                process_state(&proc.join(pid.to_string())),
                Some(ProcessState::Reaped | ProcessState::Zombie)
            ) =>
        {
            Read::Clear
        }
        Read::Unreadable if runs_as_another_user_in(proc, pid, me) => Read::Left,
        read => read,
    }
}

/// Whether `table` holds the file `key` by device and inode (`NotFound` on a closed entry is
/// clear; any other failure is unknown).
fn holds(table: Table, key: (u64, u64)) -> Read {
    let fds = match table {
        Table::Listed(fds) => fds,
        Table::Exited => return Read::Clear,
        Table::Sealed => return Read::Sealed,
        Table::Unreadable(_) => return Read::Unreadable,
    };
    let closed = |e: &io::Error| e.kind() == io::ErrorKind::NotFound;
    let mut unreadable = false;
    for entry in fds {
        let Ok(entry) = entry else {
            unreadable = true;
            continue;
        };
        match std::fs::read_link(entry.path()) {
            Ok(target) if !target.is_absolute() => continue,
            Ok(_) => {}
            Err(e) if closed(&e) => continue,
            Err(_) => {
                unreadable = true;
                continue;
            }
        }
        match std::fs::metadata(entry.path()) {
            Ok(m) if (m.dev(), m.ino()) == key => return Read::Holds,
            Ok(_) => {}
            Err(e) if closed(&e) => {}
            Err(_) => unreadable = true,
        }
    }
    if unreadable {
        Read::Unreadable
    } else {
        Read::Clear
    }
}

/// [`super::scan_file_holders`] over `proc`: each process `me` owns but `skip`, read for a
/// descriptor on `path` by its device and inode.
///
/// # Errors
///
/// `path` cannot be read (`stat`), or `proc` cannot be listed.
pub(super) fn scan_file_holders_in(
    proc: &Path,
    me: u32,
    path: &Path,
    skip: &[u32],
) -> io::Result<HolderScan> {
    let meta = std::fs::metadata(path)?;
    let key = (meta.dev(), meta.ino());
    scan_file_holders_by_key_in(proc, me, key, skip)
}

/// The descriptor pass of [`scan_file_holders_in`], with an identity read from an open file.
pub(super) fn scan_file_holders_by_key_in(
    proc: &Path,
    me: u32,
    key: (u64, u64),
    skip: &[u32],
) -> io::Result<HolderScan> {
    let mut scan = HolderScan::default();
    for pid in uid_pids_in(proc, me)? {
        if skip.contains(&pid) {
            continue;
        }
        scan.scanned += 1;
        match read_of(proc, pid, me, |table| holds(table, key)) {
            Read::Holds => scan.holders.push(pid),
            Read::Clear | Read::Left => {}
            Read::Sealed => scan.sealed.push(pid),
            Read::Unreadable => scan.unreadable.push(pid),
        }
    }
    Ok(scan)
}

/// Whether `table` holds one of the sockets `inodes`: an entry whose link names
/// `socket:[<inode>]`, read from the link alone (no `stat`). An entry closed since the
/// listing holds nothing; any other failure leaves it unknown.
fn holds_socket(table: Table, inodes: &[u64]) -> Read {
    use aterm_sysprobe::jobs::linux::{FdLink, parse_fd_link};
    let fds = match table {
        Table::Listed(fds) => fds,
        Table::Exited => return Read::Clear,
        Table::Sealed => return Read::Sealed,
        Table::Unreadable(_) => return Read::Unreadable,
    };
    let mut unreadable = false;
    for entry in fds {
        let Ok(entry) = entry else {
            unreadable = true;
            continue;
        };
        match std::fs::read_link(entry.path()) {
            Ok(link) => {
                if let FdLink::Socket(inode) = parse_fd_link(&link.to_string_lossy())
                    && inodes.contains(&inode)
                {
                    return Read::Holds;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => unreadable = true,
        }
    }
    if unreadable {
        Read::Unreadable
    } else {
        Read::Clear
    }
}

/// [`super::scan_listener_holders`] over `proc`: the `listeners` (from the kernel's socket dump)
/// `serves` accepts, and each process `me` owns but `skip` read for a descriptor on one of them.
/// With no such listener no table is read.
///
/// # Errors
///
/// `proc` cannot be listed whole.
pub(super) fn scan_listener_holders_in(
    proc: &Path,
    me: u32,
    listeners: &[UnixDiagListener],
    serves: impl Fn(&UnixDiagListener) -> bool,
    skip: &[u32],
) -> io::Result<HolderScan> {
    let inodes: Vec<u64> = listeners
        .iter()
        .filter(|listener| serves(listener))
        .map(|listener| listener.inode)
        .collect();
    let mut scan = HolderScan::default();
    for pid in uid_pids_in(proc, me)? {
        if skip.contains(&pid) {
            continue;
        }
        scan.scanned += 1;
        if inodes.is_empty() {
            continue;
        }
        match read_of(proc, pid, me, |table| holds_socket(table, &inodes)) {
            Read::Holds => scan.holders.push(pid),
            Read::Clear | Read::Left => {}
            Read::Sealed => scan.sealed.push(pid),
            Read::Unreadable => scan.unreadable.push(pid),
        }
    }
    Ok(scan)
}

/// [`super::open_files`] over `proc`: the path every entry of `pid`'s table names, in
/// descriptor order, read from the links alone (no `stat`, so no mount is waited on).
///
/// # Errors
///
/// `NotFound` when `pid` has exited or is a zombie; `PermissionDenied` when its table is
/// sealed or refused; the read's error otherwise.
pub(super) fn open_files_in(proc: &Path, pid: u32) -> io::Result<Vec<PathBuf>> {
    let fds = match table(proc, pid) {
        Table::Listed(fds) => fds,
        Table::Exited => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{pid} has exited"),
            ));
        }
        Table::Sealed => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("the kernel keeps {pid}'s descriptors from this uid"),
            ));
        }
        Table::Unreadable(e) => return Err(e),
    };
    let mut named: Vec<(u32, PathBuf)> = Vec::new();
    for entry in fds {
        let entry = entry?;
        let Some(fd) = entry
            .file_name()
            .to_str()
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse().ok())
        else {
            continue;
        };
        match std::fs::read_link(entry.path()) {
            Ok(target) if target.is_absolute() => named.push((fd, target)),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    named.sort_unstable_by_key(|(fd, _)| *fd);
    Ok(named.into_iter().map(|(_, path)| path).collect())
}

#[path = "holders_procfs_pty.rs"]
pub(super) mod pty;

#[cfg(test)]
#[path = "holders_procfs_tests.rs"]
mod tests;

/// The kernel's own `/proc`, which the fake trees above stand in for.
#[cfg(all(test, target_os = "linux"))]
#[path = "holders_procfs_live_tests.rs"]
mod live_tests;
