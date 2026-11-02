// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::processes`] and [`super::System::process_argv`] on the live system, read in
//! this process with no child: the process table in one read (`KERN_PROC_ALL`,
//! [`aterm_sysprobe::kproc::records`]), each process's executable as the kernel holds it
//! ([`aterm_uds::holders::exe_of`]), and one process's argument words only once its one thread
//! has run ([`aterm_uds::process::thread_ran`]). [`super::System::process_paths`] reads the table
//! through [`paths`].

use super::Process;
#[cfg(any(target_os = "macos", test))]
use aterm_uds::holders::Exe;
use std::ffi::OsString;
use std::path::PathBuf;

/// Every live process the kernel lists ([`listed`]); `None` where the table cannot be read (and
/// off macOS).
pub(super) fn processes() -> Option<Vec<Process>> {
    #[cfg(target_os = "macos")]
    {
        Some(listed(
            aterm_sysprobe::kproc::records()?,
            aterm_uds::holders::exe_of,
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The rows of one table read: each record's pid, parent, controlling terminal and short name,
/// with the executable `exe_of` names for it. Pid 0 (the kernel's own), a zombie and a process
/// that exited before its executable was read are left out; one the kernel names no executable
/// for keeps `exe: None`.
#[cfg(any(target_os = "macos", test))]
fn listed(
    records: Vec<aterm_sysprobe::kproc::Record>,
    mut exe_of: impl FnMut(u32) -> Exe,
) -> Vec<Process> {
    records
        .into_iter()
        .filter(|record| record.pid > 0 && !record.zombie)
        .filter_map(|record| {
            let exe = match exe_of(record.pid) {
                Exe::At(path) => Some(path),
                Exe::Unnamed => None,
                Exe::Exited => return None,
            };
            Some(Process {
                pid: record.pid,
                ppid: record.ppid,
                terminal: record.tty.is_some(),
                exe,
                comm: record.comm,
            })
        })
        .collect()
}

/// Every listed process's executable ([`super::System::process_paths`]); `None` where the table
/// was not read, or any process in it has no executable the kernel names: one unnamed process
/// keeps a live installer from disappearing into an apparently complete list.
pub(super) fn paths(rows: Option<Vec<Process>>) -> Option<Vec<PathBuf>> {
    rows?.into_iter().map(|row| row.exe).collect()
}

/// `process`'s argument words ([`super::System::process_argv`], [`argv_once_ran`]); `None` off
/// macOS.
pub(super) fn argv(process: &Process) -> Option<Vec<OsString>> {
    #[cfg(target_os = "macos")]
    {
        argv_once_ran(
            process,
            aterm_uds::process::thread_ran,
            aterm_uds::process::read_process_argv,
            aterm_uds::holders::exe_of,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = process;
        None
    }
}

/// [`argv`] over its readers: `ran` is asked first, and `read` only once it answers `Some(true)`
/// (M29); the words are kept only while `exe_of`, asked after the read, names the executable
/// `process` lists (an exec between the reads leaves no other program's words), as aterm-ctl's
/// census keeps an aterm's. `read` itself keeps them only while the same process holds the pid
/// ([`aterm_uds::process::read_process_argv`]). No read is atomic with an exec: one into a held
/// program after `ran` answered can still hang `read`, which nothing here bounds
/// (`docs/DESIGN-leftover-processes-2026-10-16.md` §6.4).
#[cfg(any(target_os = "macos", test))]
fn argv_once_ran(
    process: &Process,
    ran: impl FnOnce(i32) -> Option<bool>,
    read: impl FnOnce(i32) -> Option<Vec<OsString>>,
    exe_of: impl FnOnce(u32) -> Exe,
) -> Option<Vec<OsString>> {
    let pid = i32::try_from(process.pid).ok()?;
    if ran(pid) != Some(true) {
        return None;
    }
    let words = read(pid)?;
    let same = match exe_of(process.pid) {
        Exe::At(path) => process.exe.as_ref() == Some(&path),
        Exe::Unnamed => process.exe.is_none(),
        Exe::Exited => false,
    };
    same.then_some(words)
}

#[cfg(test)]
mod tests {
    use super::{Exe, Process};
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::path::PathBuf;

    /// The device `ttys009` is (`e_tdev`: major 16, minor 9).
    const TTYS009: i32 = (16 << 24) | 9;

    /// A record of a process launchd is the parent of, with no controlling terminal.
    fn record(pid: u32, comm: &str, zombie: bool) -> aterm_sysprobe::kproc::Record {
        aterm_sysprobe::kproc::Record {
            pid,
            ppid: 1,
            pgid: 0,
            tpgid: 0,
            tty: None,
            uid: 0,
            comm: comm.to_owned(),
            start_s: 0,
            start_us: 0,
            zombie,
        }
    }

    /// One table read keeps a process at its executable and one the kernel names none for, each
    /// with its parent and whether it has a controlling terminal (a tab's shell launchd adopted
    /// has one), and leaves out the kernel's own pid 0, a zombie and a process that exited before
    /// its executable was read. An exited process adds no executable to
    /// [`super::super::System::process_paths`]; an unnamed process keeps a live installer from
    /// disappearing into an apparently complete list. NEGATIVE CONTROLS (scratch): keeping zombies
    /// (`&& !record.zombie` dropped) fails the first assertion, and so does keeping no terminal
    /// (`terminal: record.tty.is_some()` made `terminal: false`).
    #[test]
    fn a_table_read_keeps_named_and_unnamed_processes_and_drops_the_rest() {
        let installer = PathBuf::from("/usr/sbin/softwareupdate");
        let exe_of = |pid| match pid {
            2 => Exe::At(installer.clone()),
            3 => Exe::Exited,
            4 => Exe::Unnamed,
            _ => Exe::At(PathBuf::from("/sbin/launchd")),
        };
        let rows = super::listed(
            vec![
                record(0, "kernel_task", false),
                record(2, "softwareupdate", false),
                record(3, "gone", false),
                record(4, "unlinked", false),
                record(5, "defunct", true),
                aterm_sysprobe::kproc::Record {
                    tty: Some(TTYS009),
                    ..record(6, "zsh", false)
                },
            ],
            exe_of,
        );
        assert_eq!(
            rows,
            [
                Process {
                    pid: 2,
                    ppid: 1,
                    terminal: false,
                    exe: Some(installer.clone()),
                    comm: "softwareupdate".into(),
                },
                Process {
                    pid: 4,
                    ppid: 1,
                    terminal: false,
                    exe: None,
                    comm: "unlinked".into(),
                },
                Process {
                    pid: 6,
                    ppid: 1,
                    terminal: true,
                    exe: Some(PathBuf::from("/sbin/launchd")),
                    comm: "zsh".into(),
                },
            ]
        );
        assert_eq!(super::paths(Some(rows)), None, "an unnamed process");
        let rows = super::listed(vec![record(2, "softwareupdate", false)], exe_of);
        assert_eq!(super::paths(Some(rows)), Some(vec![installer]));
        assert_eq!(super::paths(None), None, "an unread table");
    }

    /// A process's arguments are read only once its one thread has run: a program macOS's program
    /// check holds (`Some(false)`) or one whose start is unread (`None`) is never asked, and the
    /// words of one that exec'd across the read (another executable, or none named) are dropped.
    /// NEGATIVE CONTROL (scratch): reading without asking (`ran`'s answer ignored) fails the first
    /// assertion.
    #[test]
    fn arguments_are_read_only_once_the_thread_ran() {
        let row = Process {
            pid: 4242,
            ppid: 1,
            terminal: false,
            exe: Some(PathBuf::from("/Applications/Qz.app/Contents/MacOS/Qz")),
            comm: "Qz".into(),
        };
        let words = || {
            Some(vec![OsString::from(
                "/Applications/Qz.app/Contents/MacOS/Qz",
            )])
        };
        let same = || Exe::At(PathBuf::from("/Applications/Qz.app/Contents/MacOS/Qz"));
        for held in [Some(false), None] {
            let asked = Cell::new(false);
            let got = super::argv_once_ran(
                &row,
                |_| held,
                |_| {
                    asked.set(true);
                    words()
                },
                |_| same(),
            );
            assert_eq!((got, asked.get()), (None, false), "thread ran: {held:?}");
        }
        assert_eq!(
            super::argv_once_ran(&row, |pid| Some(pid == 4242), |_| words(), |_| same()),
            words()
        );
        for after in [Exe::At(PathBuf::from("/bin/sh")), Exe::Unnamed, Exe::Exited] {
            let got = super::argv_once_ran(&row, |_| Some(true), |_| words(), |_| after.clone());
            assert_eq!(got, None, "after the read: {after:?}");
        }
    }

    /// If the real table reads, it lists this process at its executable, by its own pid, under
    /// its parent's pid. NEGATIVE CONTROLS (scratch): a table read that leaves this process out
    /// fails it; one that keeps no parent (`ppid: record.ppid` made `ppid: 0`) fails the parent
    /// assertion.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_process_table_lists_this_process() {
        let Some(rows) = super::processes() else {
            return;
        };
        let own = std::env::current_exe().unwrap();
        let own = own.canonicalize().unwrap_or(own);
        let me = rows.iter().find(|row| row.pid == std::process::id());
        assert!(
            me.and_then(|row| row.exe.as_ref()).is_some_and(|exe| exe
                .canonicalize()
                .as_ref()
                .unwrap_or(exe)
                == &own),
            "{} not listed at {} among {} processes: {me:?}",
            std::process::id(),
            own.display(),
            rows.len()
        );
        assert_eq!(
            me.map(|row| row.ppid),
            Some(std::os::unix::process::parent_id()),
            "its parent"
        );
    }
}
