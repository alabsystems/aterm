// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux's descriptor tables (`<proc>/<pid>/fd`), answering [`super::scan_file_holders`],
//! [`super::scan_listener_holders`] (with `<proc>/net/unix`) and [`super::open_files`] from
//! `/proc` in production or a scratch tree in tests.
//!
//! Two kinds of same-uid process have root-owned tables (`task_dump_owner` in `fs/proc/base.c`):
//! zombies holding nothing ([`Table::Exited`]), and non-dumpable live processes such as
//! `ssh-agent` ([`Table::Sealed`]); any other refusal is [`Table::Unreadable`], which a scan
//! counts outside it once the process runs as another user (`read_of`).

use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::HolderScan;

/// The kernel's procfs.
#[cfg(target_os = "linux")]
pub(super) const PROC: &str = "/proc";

/// How long a scan may take before a hung mount makes its answer unknown.
#[cfg(target_os = "linux")]
pub(super) const SCAN_DEADLINE: Duration = Duration::from_secs(10);

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
    let stat = match crate::process::read_proc_text(&dir.join("stat")) {
        Ok(stat) => stat,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(ProcessState::Reaped),
        Err(_) => return None,
    };
    let state = stat.rsplit_once(')')?.1.trim_start().chars().next()?;
    match state {
        'Z' | 'X' | 'x' => {}
        'R' | 'S' | 'D' | 'T' | 't' | 'W' | 'K' | 'P' | 'I' => return Some(ProcessState::Live),
        _ => return None,
    }
    let threads = crate::process::read_proc_text(&dir.join("status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|l| l.strip_prefix("Threads:"))
                .and_then(|n| n.trim().parse::<u32>().ok())
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

/// Every pid under `proc` that runs as effective uid `me`: its directory's owner. The kernel
/// keeps `/proc/<pid>` the effective uid's when the process is non-dumpable and gives root only
/// the entries inside (`task_dump_owner` spares a `0555` directory, as
/// `docs/measured/linux-lane-2026-10-23.md` records).
///
/// # Errors
///
/// `proc` could not be listed.
pub(super) fn uid_pids_in(proc: &Path, me: u32) -> io::Result<Vec<u32>> {
    Ok(std::fs::read_dir(proc)?
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            (std::fs::metadata(entry.path()).ok()?.uid() == me).then_some(pid)
        })
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

/// Whether `<proc>/<pid>` names another effective user than `me` now: its directory's owner,
/// which the kernel keeps the process's effective uid ([`uid_pids_in`]). `false` once the
/// directory is gone.
pub(super) fn runs_as_another_user_in(proc: &Path, pid: u32, me: u32) -> bool {
    std::fs::metadata(proc.join(pid.to_string())).is_ok_and(|m| m.uid() != me)
}

/// What `read` makes of `pid`'s table, an unknown answer read [`Read::Left`] once `pid` runs as
/// another effective user than `me` ([`runs_as_another_user_in`]). A scan lists only `me`'s
/// processes, so a process that execs a set-user-ID program (`sudo`, `su`, `passwd`) between
/// the listing and its read is outside it, as it would be listed a moment later: the exec makes
/// its directory and its table root's (`task_dump_owner`, the process now non-dumpable), so its
/// table is refused and would otherwise leave the whole scan unknown.
fn read_of(proc: &Path, pid: u32, me: u32, read: impl FnOnce(Table) -> Read) -> Read {
    match read(table(proc, pid)) {
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

/// [`super::scan_listener_holders`] over `proc`: the listening sockets `<proc>/net/unix`
/// lists at a path `names` accepts, and each process `me` owns but `skip` read for a
/// descriptor on one of them. With no such listener no table is read.
///
/// # Errors
///
/// `<proc>/net/unix` or `proc` cannot be read.
pub(super) fn scan_listener_holders_in(
    proc: &Path,
    me: u32,
    names: impl Fn(&[u8]) -> bool,
    skip: &[u32],
) -> io::Result<HolderScan> {
    let net_unix = std::fs::read(proc.join("net/unix"))?;
    let inodes: Vec<u64> = aterm_sysprobe::jobs::linux::listening_unix(&net_unix)
        .into_iter()
        .filter(|(_, bound)| names(bound))
        .map(|(inode, _)| inode)
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
        let Some(fd) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
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

/// `read`'s answer if it gives one within `deadline`, read on a thread of its own; else
/// `TimedOut`. A read that does not answer keeps its thread (no read can be cancelled).
///
/// # Errors
///
/// `read`'s error, `TimedOut`, or the thread not starting.
pub(super) fn within<T: Send + 'static>(
    deadline: Duration,
    read: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let (answer, answered) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("aterm-holder-scan".to_string())
        .spawn(move || {
            let _ = answer.send(read());
        })?;
    match answered.recv_timeout(deadline) {
        Ok(read) => read,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "the descriptor tables did not answer within {} s (a hung mount)",
                deadline.as_secs()
            ),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("the descriptor scan ended with no answer"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::{
        Read, Table, holds, open_files_in, read_of, runs_as_another_user_in, scan_file_holders_in,
        table, uid_pids_in, within,
    };

    /// A directory only root may list, which a sealed or zombie table's `fd` stands in for
    /// (the kernel makes those root's): macOS's `/var/root`, Linux's `/root`.
    fn roots_own() -> PathBuf {
        crate::host::could_not_run_as_root("a directory only root may list");
        ["/var/root", "/root"]
            .iter()
            .map(PathBuf::from)
            .find(|d| {
                std::fs::metadata(d).is_ok_and(|m| {
                    use std::os::unix::fs::MetadataExt as _;
                    m.uid() == 0
                }) && std::fs::read_dir(d).is_err()
            })
            .unwrap_or_else(|| {
                crate::host::could_not_run(
                    "this host has no root-owned directory this uid may not list",
                )
            })
    }

    /// A process directory under `proc`: `state` its `stat`'s, `threads` its `status`'s, and
    /// `fd` its table's entries (`n` and what each link names), or `None` to make `fd` a link
    /// to `sealed` (a table the kernel made root's).
    fn process(
        proc: &Path,
        pid: u32,
        state: char,
        threads: u32,
        fd: Option<&[(u32, &str)]>,
        sealed: &Path,
    ) {
        let dir = proc.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("stat"),
            format!("{pid} (a (b) c) {state} 1 {pid} {pid} 0 -1"),
        )
        .unwrap();
        std::fs::write(
            dir.join("status"),
            format!("Name:\tx\nState:\t{state}\nThreads:\t{threads}\n"),
        )
        .unwrap();
        match fd {
            Some(entries) => {
                std::fs::create_dir(dir.join("fd")).unwrap();
                for (n, to) in entries {
                    symlink(to, dir.join("fd").join(n.to_string())).unwrap();
                }
            }
            None => symlink(sealed, dir.join("fd")).unwrap(),
        }
    }

    fn me() -> u32 {
        crate::peer::our_uid()
    }

    /// Holders are read from each table by device and inode; non-dumpable live processes are
    /// sealed, zombies and reaped processes are clear, and other refused or missing tables are
    /// unknown.
    #[test]
    fn a_tables_holders_are_its_descriptors_on_the_file() {
        let sealed = roots_own();
        let tmp = aterm_tempfile::tempdir().unwrap();
        let (proc, files) = (tmp.path().join("proc"), tmp.path().join("files"));
        std::fs::create_dir_all(&files).unwrap();
        let (lock, other) = (files.join("lock"), files.join("other"));
        std::fs::write(&lock, b"").unwrap();
        std::fs::write(&other, b"").unwrap();
        let alias = files.join("alias");
        symlink(&lock, &alias).unwrap();
        let s = |p: &Path| p.to_str().unwrap().to_owned();
        let (lock_s, other_s, alias_s) = (s(&lock), s(&other), s(&alias));
        process(
            &proc,
            101,
            'S',
            4,
            Some(&[(0, "pipe:[77]"), (1, "/dev/null"), (3, &lock_s)]),
            &sealed,
        );
        process(
            &proc,
            102,
            'S',
            1,
            Some(&[(3, &other_s), (4, "socket:[5]")]),
            &sealed,
        );
        process(&proc, 103, 'Z', 1, None, &sealed);
        process(&proc, 104, 'S', 1, None, &sealed);
        process(&proc, 105, 'Z', 3, None, &sealed);
        process(&proc, 106, 'S', 1, Some(&[]), &sealed);
        std::fs::set_permissions(proc.join("106/fd"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        process(
            &proc,
            107,
            'R',
            1,
            Some(&[(9, "/no/such/file/anywhere")]),
            &sealed,
        );
        process(&proc, 108, 'S', 1, Some(&[(5, &alias_s)]), &sealed);
        process(&proc, 109, 'S', 1, Some(&[(5, &lock_s)]), &sealed);
        for (pid, state, threads) in [(110, 'Z', 3), (111, 'S', 1), (112, 'Z', 1), (113, 'Z', 3)] {
            process(&proc, pid, state, threads, Some(&[]), &sealed);
            std::fs::remove_dir(proc.join(pid.to_string()).join("fd")).unwrap();
        }
        std::fs::remove_file(proc.join("113/status")).unwrap();
        process(&proc, 114, 'Z', 3, None, &sealed);
        std::fs::remove_file(proc.join("114/status")).unwrap();
        process(&proc, 115, '?', 1, None, &sealed);
        std::fs::create_dir(proc.join("self")).unwrap();

        let mut scan = scan_file_holders_in(&proc, me(), &lock, &[109]).expect("scan");
        std::fs::set_permissions(proc.join("106/fd"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        for list in [&mut scan.holders, &mut scan.unreadable, &mut scan.sealed] {
            list.sort_unstable();
        }
        assert_eq!(scan.holders, [101, 108], "{scan:?}");
        assert_eq!(scan.sealed, [104], "{scan:?}");
        assert_eq!(
            scan.unreadable,
            [105, 106, 110, 111, 113, 114, 115],
            "{scan:?}"
        );
        assert_eq!(scan.scanned, 14, "109 skipped, `self` no pid");
        assert!(matches!(table(&proc, 112), Table::Exited), "a lone zombie");
        assert!(matches!(table(&proc, 299), Table::Exited), "reaped");
        let by_other = scan_file_holders_in(&proc, me(), &other, &[]).expect("scan");
        assert!(by_other.holders.contains(&102) && !by_other.holders.contains(&101));
        assert!(scan_file_holders_in(&proc, me(), &files.join("absent"), &[]).is_err());
        assert!(
            uid_pids_in(&proc, me().wrapping_add(1)).unwrap().is_empty(),
            "another uid's"
        );
    }

    /// A table refused once its process runs as another effective user than the scan's (a
    /// set-user-ID exec since the listing, which makes `/proc/<pid>` that user's) reads `Left`,
    /// outside the scan. Negative control: the same refusal scanned as its directory's owner
    /// stays unknown. A table read or sealed keeps its answer, and a process gone runs as no one.
    #[test]
    fn a_table_refused_once_its_process_runs_as_another_user_is_outside_the_scan() {
        use std::os::unix::fs::MetadataExt as _;
        let sealed = roots_own();
        let tmp = aterm_tempfile::tempdir().unwrap();
        let (proc, held) = (tmp.path().join("proc"), tmp.path().join("held"));
        std::fs::create_dir_all(&proc).unwrap();
        std::fs::write(&held, b"").unwrap();
        let meta = std::fs::metadata(&held).unwrap();
        let key = (meta.dev(), meta.ino());
        let held_s = held.to_str().unwrap().to_owned();
        process(&proc, 401, 'S', 1, Some(&[]), &sealed);
        std::fs::set_permissions(proc.join("401/fd"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        process(&proc, 402, 'S', 1, Some(&[(3, &held_s)]), &sealed);
        process(&proc, 403, 'S', 1, None, &sealed);
        let (me, other) = (me(), me().wrapping_add(1));
        let read = |pid, scan_as| read_of(&proc, pid, scan_as, |table| holds(table, key));
        let reads = [
            read(401, me),
            read(401, other),
            read(402, other),
            read(403, other),
        ];
        std::fs::set_permissions(proc.join("401/fd"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert_eq!(
            reads,
            [Read::Unreadable, Read::Left, Read::Holds, Read::Sealed]
        );
        assert!(!runs_as_another_user_in(&proc, 401, me));
        assert!(runs_as_another_user_in(&proc, 401, other));
        assert!(
            !runs_as_another_user_in(&proc, 499, other),
            "a process gone"
        );
    }

    /// `stat` and `status` are read lossily ([`crate::process::read_proc_text`]), so a table
    /// whose command name is no UTF-8 is judged by its state like any other. Negative control:
    /// read with `read_to_string`, and the lone zombie's and the live process's are unknown.
    #[test]
    fn a_table_whose_command_name_is_not_utf8_is_judged_by_its_state() {
        let sealed = roots_own();
        let tmp = aterm_tempfile::tempdir().unwrap();
        let proc = tmp.path().join("proc");
        for (pid, state, threads) in [(301, 'Z', 1), (302, 'S', 1), (303, 'Z', 3)] {
            process(&proc, pid, state, threads, None, &sealed);
            let dir = proc.join(pid.to_string());
            for name in ["stat", "status"] {
                let mut text = std::fs::read(dir.join(name)).unwrap();
                let at = text.iter().position(|&b| b == b'x' || b == b'a').unwrap();
                text.splice(at..=at, [0xe3, 0x83]);
                std::fs::write(dir.join(name), &text).unwrap();
                assert!(
                    std::fs::read_to_string(dir.join(name)).is_err(),
                    "the fixture's {name} names no UTF-8"
                );
            }
        }
        assert!(matches!(table(&proc, 301), Table::Exited), "a lone zombie");
        assert!(matches!(table(&proc, 302), Table::Sealed), "a live process");
        assert!(
            matches!(table(&proc, 303), Table::Unreadable(_)),
            "a zombie leader whose other threads run"
        );
    }

    /// A listener is held by the process whose table names its socket; a stream accepted at its
    /// path, another listener and a zombie hold nothing, and a sealed table is sealed. A path no
    /// listener is bound at reads no table. `net/unix` is read as bytes, so a path that is no
    /// UTF-8 is a row like any other, and a missing `net/unix` is an error.
    #[test]
    fn a_listener_is_held_by_the_process_whose_table_names_its_socket() {
        let sealed = roots_own();
        let tmp = aterm_tempfile::tempdir().unwrap();
        let proc = tmp.path().join("proc");
        std::fs::create_dir_all(proc.join("net")).unwrap();
        std::fs::write(
            proc.join("net/unix"),
            b"Num       RefCount Protocol Flags    Type St Inode Path\n\
             0000000000000000: 00000002 00000000 00010000 0001 01 41001 /run/b.sock\n\
             0000000000000000: 00000003 00000000 00000000 0001 03 41002 /run/b.sock\n\
             0000000000000000: 00000002 00000000 00010000 0001 01 41004 /run/\xff.sock\n\
             0000000000000000: 00000002 00000000 00010000 0001 01 41003 /run/other.sock\n",
        )
        .unwrap();
        process(
            &proc,
            301,
            'S',
            1,
            Some(&[(0, "/dev/null"), (7, "socket:[41001]")]),
            &sealed,
        );
        process(&proc, 302, 'S', 1, Some(&[(4, "socket:[41002]")]), &sealed);
        process(&proc, 303, 'S', 1, Some(&[(4, "socket:[41003]")]), &sealed);
        process(&proc, 304, 'Z', 1, None, &sealed);
        process(&proc, 305, 'S', 1, None, &sealed);
        let at = |path: &'static [u8]| move |bound: &[u8]| bound == path;
        let scan = super::scan_listener_holders_in(&proc, me(), at(b"/run/b.sock"), &[]).unwrap();
        assert_eq!(scan.holders, [301], "{scan:?}");
        assert_eq!(scan.sealed, [305], "{scan:?}");
        assert!(scan.unreadable.is_empty(), "{scan:?}");
        assert_eq!(scan.scanned, 5);
        let none = super::scan_listener_holders_in(&proc, me(), at(b"/run/c.sock"), &[]).unwrap();
        assert!(
            none.holders.is_empty() && none.sealed.is_empty(),
            "{none:?}"
        );
        assert_eq!(none.scanned, 5);
        std::fs::remove_file(proc.join("net/unix")).unwrap();
        assert!(super::scan_listener_holders_in(&proc, me(), at(b"/run/b.sock"), &[]).is_err());
    }

    /// Open files are the paths a table's links name in descriptor order without `stat`;
    /// zombies return `NotFound`, sealed tables `PermissionDenied`, and other refusals their
    /// own error.
    #[test]
    fn a_processs_open_files_are_the_paths_its_links_name() {
        let sealed = roots_own();
        let tmp = aterm_tempfile::tempdir().unwrap();
        let proc = tmp.path().join("proc");
        process(
            &proc,
            201,
            'S',
            1,
            Some(&[
                (10, "/w/b"),
                (2, "pipe:[1]"),
                (3, "/w/a (deleted)"),
                (1, "/dev/null"),
            ]),
            &sealed,
        );
        process(&proc, 202, 'Z', 1, None, &sealed);
        process(&proc, 203, 'S', 1, None, &sealed);
        process(&proc, 204, 'S', 1, Some(&[]), &sealed);
        let want: Vec<PathBuf> = ["/dev/null", "/w/a (deleted)", "/w/b"]
            .map(PathBuf::from)
            .into();
        assert_eq!(open_files_in(&proc, 201).expect("read"), want);
        let kind = |pid| open_files_in(&proc, pid).map_err(|e| e.kind());
        assert_eq!(kind(202), Err(std::io::ErrorKind::NotFound), "a zombie");
        assert_eq!(kind(299), Err(std::io::ErrorKind::NotFound), "reaped");
        assert_eq!(
            kind(203),
            Err(std::io::ErrorKind::PermissionDenied),
            "sealed"
        );
        std::fs::set_permissions(proc.join("204/fd"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let refused = kind(204);
        std::fs::set_permissions(proc.join("204/fd"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert_eq!(refused, Err(std::io::ErrorKind::PermissionDenied));
        assert!(
            matches!(table(&proc, 204), Table::Listed(_)),
            "readable again"
        );
    }

    /// A pid's uid is its directory's owner, never `status`'s `Uid:` line (`live_tests` holds
    /// the kernel to that). Negative control: judged by `Uid:`, the other uid is handed this
    /// uid's process.
    #[test]
    fn a_process_is_the_uid_its_directorys_owner_names() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let proc = tmp.path().join("proc");
        process(&proc, 301, 'S', 1, Some(&[]), tmp.path());
        std::fs::create_dir(proc.join("self")).unwrap();
        let other = me().wrapping_add(1);
        let status = proc.join("301/status");
        let text = std::fs::read_to_string(&status).unwrap();
        std::fs::write(
            &status,
            format!("{text}Uid:\t{other}\t{other}\t{other}\t{other}\n"),
        )
        .unwrap();
        assert_eq!(uid_pids_in(&proc, me()).unwrap(), [301]);
        assert!(
            uid_pids_in(&proc, other).unwrap().is_empty(),
            "a status names no owner"
        );
    }

    /// A read held past the deadline returns `TimedOut`, and one that finishes returns its
    /// result.
    #[test]
    fn a_scan_that_does_not_answer_in_time_is_unknown() {
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let held = within(Duration::from_millis(50), move || {
            let _ = wait.recv();
            Ok(7)
        });
        assert_eq!(
            held.map_err(|e| e.kind()),
            Err(std::io::ErrorKind::TimedOut)
        );
        drop(go);
        let answered = within(Duration::from_secs(60), || Ok(7));
        assert_eq!(answered.expect("answered"), 7);
        let failed = within(Duration::from_secs(60), || -> std::io::Result<u8> {
            Err(std::io::Error::other("its own"))
        });
        assert_eq!(failed.expect_err("its error").to_string(), "its own");
    }
}

/// The kernel's own `/proc`, which the fake trees above stand in for.
#[cfg(all(test, target_os = "linux"))]
mod live_tests {
    use std::io::ErrorKind;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::{PROC, Table, table};

    /// The hang detector for a child's exec.
    const HANG: Duration = Duration::from_secs(60);

    /// A running copy of `/bin/sleep`, killed and reaped when it drops, so a failed assertion
    /// or a hang detector leaves nothing running.
    struct Sleeper(crate::spawnfd::Child);

    impl Sleeper {
        /// A copy of `/bin/sleep` at `copy` with `mode` and `held` as its stdin, returned once
        /// its exec is done: the kernel names the task after the copy (`comm`) only after it has
        /// settled the task's dumpability (`begin_new_exec`).
        fn start(copy: &Path, mode: u32, held: &Path) -> Self {
            std::fs::copy("/bin/sleep", copy).expect("copy /bin/sleep");
            std::fs::set_permissions(copy, std::fs::Permissions::from_mode(mode)).expect("chmod");
            let file = std::fs::File::open(held).expect("open the held file");
            let sleeper = Self(
                crate::spawnfd::command(copy)
                    .arg("600")
                    .stdin(crate::spawnfd::Stdio::from(file))
                    .stdout(crate::spawnfd::Stdio::null())
                    .stderr(crate::spawnfd::Stdio::null())
                    .spawn()
                    .expect("spawn the copy"),
            );
            let name = copy.file_name().expect("a file name").as_encoded_bytes();
            let comm = format!("{PROC}/{}/comm", sleeper.id());
            let started = Instant::now();
            while !std::fs::read(&comm).is_ok_and(|c| c.trim_ascii_end() == name) {
                assert!(
                    started.elapsed() < HANG,
                    "{} was never exec'd",
                    copy.display()
                );
                std::thread::yield_now();
            }
            sleeper
        }

        fn id(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A child holding a file is its holder and names it. A copy this uid may run but not read
    /// is non-dumpable (as `ssh-agent` makes itself), so its table is root's: sealed, not
    /// unreadable, and still this uid's by its directory's owner. COULD NOT RUN where
    /// `fs.suid_dumpable = 1` keeps such an exec dumpable. Negative control: the readable copy
    /// of the same program on the same file is listed and holds it.
    #[test]
    fn the_kernels_tables_name_a_holder_and_seal_a_non_dumpable_one() {
        crate::host::could_not_run_as_root("a program this uid may run but not read");
        let tmp = aterm_tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let (held, unheld) = (dir.join("held"), dir.join("unheld"));
        std::fs::write(&held, b"held").unwrap();
        std::fs::write(&unheld, b"").unwrap();
        let me = std::process::id();

        let open = Sleeper::start(&dir.join("readable-sleep"), 0o755, &held);
        let sealed = Sleeper::start(&dir.join("sealed-sleep"), 0o111, &held);
        let (o, s) = (open.id(), sealed.id());
        let owner = |p: String| std::fs::metadata(p).map(|m| m.uid()).ok();
        if owner(format!("{PROC}/{s}/fd")) != Some(0) {
            let setting = std::fs::read_to_string("/proc/sys/fs/suid_dumpable")
                .map_or_else(|e| format!("unread: {e}"), |v| v.trim().to_owned());
            crate::host::could_not_run(format!(
                "this kernel kept a program this uid may run but not read dumpable, so its table \
                 stays this uid's and nothing is sealed (fs.suid_dumpable = {setting}; 1 keeps \
                 such an exec dumpable, 0 and 2 do not)"
            ));
        }

        let scan = super::super::scan_file_holders(&held, &[me]).expect("scan");
        assert!(scan.holds(o), "the readable copy: {scan:?}");
        assert!(
            !scan.sealed.contains(&o) && !scan.unreadable.contains(&o),
            "{scan:?}"
        );
        assert!(scan.sealed.contains(&s), "the non-dumpable copy: {scan:?}");
        assert!(!scan.holds(s) && !scan.unreadable.contains(&s), "{scan:?}");
        let other = super::super::scan_file_holders(&unheld, &[me]).expect("scan");
        assert!(!other.holds(o), "a file it does not hold: {other:?}");
        assert!(matches!(table(Path::new(PROC), o), Table::Listed(_)));
        assert!(matches!(table(Path::new(PROC), s), Table::Sealed));
        let files = super::super::open_files(o).expect("its open files");
        assert!(files.contains(&held), "{files:?}");
        assert_eq!(
            super::super::open_files(s).map_err(|e| e.kind()),
            Err(ErrorKind::PermissionDenied)
        );
        assert_eq!(
            owner(format!("{PROC}/{s}")),
            Some(crate::peer::our_uid()),
            "a non-dumpable process's directory stays its effective uid's"
        );
        let ours = super::super::uid_pids().expect("listing");
        assert!(ours.contains(&o) && ours.contains(&s), "{ours:?}");
    }

    /// A running program this test holds stdin of, killed and reaped when it drops, so a failed
    /// assertion or a hang detector leaves nothing running.
    struct Held(crate::spawnfd::Child);

    impl Drop for Held {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A process of this uid's that execs a set-user-ID program (`su`, left waiting for a
    /// password on a pipe this test holds) runs as root: the kernel makes its directory and its
    /// table root's, so the table alone is unknown, not sealed, and [`super::read_of`] counts
    /// it outside a scan of this uid, whose listing no longer names it. `su` is killed while it
    /// waits, before it reads a password. COULD NOT RUN without a set-user-ID root
    /// `/usr/bin/su`. Negative control: [`table`] alone answers `Unreadable`.
    #[test]
    fn a_process_that_became_a_set_user_id_program_reads_left_not_unknown() {
        crate::host::could_not_run_as_root("a set-user-ID program that runs as another user");
        let su = Path::new("/usr/bin/su");
        if !std::fs::metadata(su).is_ok_and(|m| m.uid() == 0 && m.mode() & 0o4000 != 0) {
            crate::host::could_not_run("this host has no set-user-ID root /usr/bin/su");
        }
        let tmp = aterm_tempfile::tempdir().unwrap();
        let held = tmp.path().join("held");
        std::fs::write(&held, b"").unwrap();
        let meta = std::fs::metadata(&held).unwrap();
        let key = (meta.dev(), meta.ino());
        let me = crate::peer::our_uid();

        let su = Held(
            crate::spawnfd::command(su)
                .args(["root", "-c", "true"])
                .stdin(crate::spawnfd::Stdio::piped())
                .stdout(crate::spawnfd::Stdio::null())
                .stderr(crate::spawnfd::Stdio::null())
                .spawn()
                .expect("spawn su"),
        );
        let pid = su.0.id();
        let dir = format!("{PROC}/{pid}");
        let started = Instant::now();
        while std::fs::metadata(&dir).map(|m| m.uid()).ok() != Some(0) {
            assert!(started.elapsed() < HANG, "su never ran as root");
            std::thread::yield_now();
        }

        let read = super::read_of(Path::new(PROC), pid, me, |table| super::holds(table, key));
        let alone = table(Path::new(PROC), pid);
        let listed = super::uid_pids_in(Path::new(PROC), me).expect("listing");
        drop(su);
        assert_eq!(read, super::Read::Left, "outside this uid's scan");
        assert!(matches!(alone, Table::Unreadable(_)), "{alone:?}");
        assert!(!listed.contains(&pid), "no longer listed as this uid's");
    }
}
