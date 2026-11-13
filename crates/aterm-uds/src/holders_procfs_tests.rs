// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    Read, Table, euid_in, holds, open_files_in, pids_listed, read_of, runs_as_another_user_in,
    scan_file_holders_in, table, uid_pids_in,
};
use crate::holders::within;

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
    std::fs::set_permissions(proc.join("106/fd"), std::fs::Permissions::from_mode(0o000)).unwrap();
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
    std::fs::create_dir(proc.join("+101")).unwrap();

    let mut scan = scan_file_holders_in(&proc, me(), &lock, &[109]).expect("scan");
    std::fs::set_permissions(proc.join("106/fd"), std::fs::Permissions::from_mode(0o700)).unwrap();
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
/// A listed table whose entry refuses a read once its process exited holds nothing; while
/// the process runs, the same refusal stays unknown. NEGATIVE CONTROL: judge the refusal by
/// the user alone, and the exited processes' tables read unknown.
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
    std::fs::set_permissions(proc.join("401/fd"), std::fs::Permissions::from_mode(0o000)).unwrap();
    process(&proc, 402, 'S', 1, Some(&[(3, &held_s)]), &sealed);
    process(&proc, 403, 'S', 1, None, &sealed);
    // Listed while it ran, its entry refused since (no symbolic link reads `EINVAL`, as the
    // kernel's refusal of an exited process's entry reads `EACCES`): a lone zombie and a
    // process reaped hold nothing, a zombie leader whose other threads run stays unknown.
    for (pid, state, threads) in [(404, 'Z', 1), (405, 'S', 1), (406, 'Z', 3), (407, 'Z', 1)] {
        process(&proc, pid, state, threads, Some(&[]), &sealed);
        std::fs::create_dir(proc.join(pid.to_string()).join("fd/3")).unwrap();
    }
    std::fs::remove_file(proc.join("407/stat")).unwrap();
    let (me, other) = (me(), me().wrapping_add(1));
    let read = |pid, scan_as| read_of(&proc, pid, scan_as, |table| holds(table, key));
    let reads = [
        read(401, me),
        read(401, other),
        read(402, other),
        read(403, other),
    ];
    let exited = [read(404, me), read(405, me), read(406, me), read(407, me)];
    std::fs::set_permissions(proc.join("401/fd"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        reads,
        [Read::Unreadable, Read::Left, Read::Holds, Read::Sealed]
    );
    assert_eq!(
        exited,
        [Read::Clear, Read::Unreadable, Read::Unreadable, Read::Clear],
        "a lone zombie, a live process, a zombie leader whose threads run, a reaped process"
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

/// A `stat` read that answers `ESRCH`, a process reaped between the open and the read, is a
/// reaped process, as a `stat` gone by its open (`ENOENT`) is, and no `status` is read for
/// either; any other failed read is unknown. NEGATIVE CONTROL: judge only `ENOENT` gone, and
/// the `ESRCH` read is unknown, so a scan counts that process unreadable.
#[test]
fn a_stat_read_that_finds_the_process_reaped_is_reaped() {
    use super::{ProcessState, state_of};
    let unasked = || -> Option<String> { panic!("a reaped process's status was read") };
    for gone in [
        std::io::Error::from(std::io::ErrorKind::NotFound),
        std::io::Error::from_raw_os_error(libc::ESRCH),
    ] {
        let why = gone.to_string();
        assert_eq!(
            state_of(Err(gone), unasked),
            Some(ProcessState::Reaped),
            "{why}"
        );
    }
    assert_eq!(
        state_of(
            Err(std::io::Error::from_raw_os_error(libc::EACCES)),
            unasked
        ),
        None,
        "a refused read"
    );
}

/// A listener is held by the process whose table names its socket: a stream accepted at the
/// listener's path (no listener row) and another listener hold nothing, a zombie holds
/// nothing, and a sealed table is sealed. Another socket's name that is no UTF-8 is a row
/// like any other, and a path no listener is bound at reads no table. NEGATIVE CONTROL: keep
/// every listener whatever its path, and the process holding another listener joins.
#[test]
fn a_listener_is_held_by_the_process_whose_table_names_its_socket() {
    use aterm_sysprobe::linux::UnixDiagListener;
    let sealed = roots_own();
    let tmp = aterm_tempfile::tempdir().unwrap();
    let proc = tmp.path().join("proc");
    let listeners: Vec<UnixDiagListener> = [
        (41001, &b"/run/b.sock"[..]),
        (41004, b"/run/\xff.sock"),
        (41003, b"/run/other.sock"),
    ]
    .map(|(inode, path)| UnixDiagListener {
        inode,
        path: path.to_vec(),
        file: None,
    })
    .to_vec();
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
    let at = |path: &'static [u8]| move |bound: &UnixDiagListener| bound.path == path;
    let scan =
        super::scan_listener_holders_in(&proc, me(), &listeners, at(b"/run/b.sock"), &[]).unwrap();
    assert_eq!(scan.holders, [301], "{scan:?}");
    assert_eq!(scan.sealed, [305], "{scan:?}");
    assert!(scan.unreadable.is_empty(), "{scan:?}");
    assert_eq!(scan.scanned, 5);
    let none =
        super::scan_listener_holders_in(&proc, me(), &listeners, at(b"/run/c.sock"), &[]).unwrap();
    assert!(
        none.holders.is_empty() && none.sealed.is_empty(),
        "{none:?}"
    );
    assert_eq!(none.scanned, 5);
}

/// A listing is the positive numbers its entries name in digits alone, any other name (a
/// sign included) skipped, and an entry that does not read refuses the whole listing.
/// NEGATIVE CONTROL: skip such an entry, and the listing cut short reads as whole.
#[test]
fn a_listing_with_an_entry_that_does_not_read_is_refused() {
    use std::io::{Error, ErrorKind};
    assert_eq!(
        pids_listed(["10", "self", "20", "-1", "0", "+30"].map(|s| Ok(s.into()))).unwrap(),
        [10, 20]
    );
    let cut = pids_listed([
        Ok("10".into()),
        Err(Error::from(ErrorKind::PermissionDenied)),
        Ok("20".into()),
    ]);
    assert!(cut.is_err(), "{cut:?}");
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
    symlink("/w/ignored", proc.join("201/fd/+4")).unwrap();
    assert_eq!(open_files_in(&proc, 201).expect("read"), want);
    let kind = |pid| open_files_in(&proc, pid).map_err(|e| e.kind());
    assert_eq!(kind(202), Err(std::io::ErrorKind::NotFound), "a zombie");
    assert_eq!(kind(299), Err(std::io::ErrorKind::NotFound), "reaped");
    assert_eq!(
        kind(203),
        Err(std::io::ErrorKind::PermissionDenied),
        "sealed"
    );
    std::fs::set_permissions(proc.join("204/fd"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let refused = kind(204);
    std::fs::set_permissions(proc.join("204/fd"), std::fs::Permissions::from_mode(0o700)).unwrap();
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
    std::fs::create_dir(proc.join("+101")).unwrap();
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
    assert_eq!(euid_in(&proc, 301).unwrap(), me());
    assert_eq!(
        euid_in(&proc, 999).map_err(|e| e.kind()),
        Err(std::io::ErrorKind::NotFound),
        "a pid gone"
    );
}

/// A read held past the deadline returns `TimedOut`, and one that finishes returns its
/// result.
#[test]
fn a_scan_that_does_not_answer_in_time_is_unknown() {
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let held = within(Duration::from_millis(50), "a read", "held", move || {
        let _ = wait.recv();
        Ok(7)
    });
    assert_eq!(
        held.map_err(|e| e.kind()),
        Err(std::io::ErrorKind::TimedOut)
    );
    drop(go);
    let answered = within(Duration::from_secs(60), "a read", "held", || Ok(7));
    assert_eq!(answered.expect("answered"), 7);
    let failed = within(
        Duration::from_secs(60),
        "a read",
        "held",
        || -> std::io::Result<u8> { Err(std::io::Error::other("its own")) },
    );
    assert_eq!(failed.expect_err("its error").to_string(), "its own");
}
