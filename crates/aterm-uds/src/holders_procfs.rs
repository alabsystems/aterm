// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux's descriptor tables (`<proc>/<pid>/fd`), answering [`super::scan_file_holders`] and
//! [`super::open_files`] from `/proc` in production or a scratch tree in tests.
//!
//! Two kinds of same-uid process have root-owned tables (`task_dump_owner` in `fs/proc/base.c`):
//! zombies holding nothing ([`Table::Exited`]), and non-dumpable live processes such as
//! `ssh-agent` ([`Table::Sealed`]); any other refusal is [`Table::Unreadable`].

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
/// dead task, its thread count in `status`; `None` when either required read is unknown.
fn process_state(dir: &Path) -> Option<ProcessState> {
    let stat = match std::fs::read_to_string(dir.join("stat")) {
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
    let threads = std::fs::read_to_string(dir.join("status"))
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

/// Every pid under `proc` that runs as effective uid `me`, judged by its directory's owner
/// and, for a directory `me` does not own, its status ([`super::ours_by_owner`]).
///
/// # Errors
///
/// `proc` could not be listed.
pub(super) fn uid_pids_in(proc: &Path, me: u32) -> io::Result<Vec<u32>> {
    Ok(std::fs::read_dir(proc)?
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let owner = std::fs::metadata(entry.path()).ok()?.uid();
            let effective = || {
                crate::process::read_proc_text(&entry.path().join("status"))
                    .ok()
                    .and_then(|status| crate::process::procfs::effective_uid(&status))
            };
            super::ours_by_owner(owner, me, effective).then_some(pid)
        })
        .collect())
}

/// What one table says of the file whose device and inode are `key`.
#[derive(Debug, PartialEq, Eq)]
enum Read {
    Holds,
    Clear,
    Sealed,
    Unreadable,
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
        match holds(table(proc, pid), key) {
            Read::Holds => scan.holders.push(pid),
            Read::Clear => {}
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

    use super::{Table, open_files_in, scan_file_holders_in, table, uid_pids_in, within};

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
