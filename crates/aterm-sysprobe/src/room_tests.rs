// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The task tables' readers: Linux's on a fixture tree (every host), this Mac's live.

use super::*;

#[test]
fn file_nr_reads_the_allocated_less_the_free() {
    assert_eq!(file_nr_open("8192\t0\t9223372036854775807\n"), Some(8192));
    assert_eq!(file_nr_open("8192 192 400000"), Some(8000));
    assert_eq!(file_nr_open("8192 0"), None, "two fields");
    assert_eq!(file_nr_open("8192 0 400000 7"), None, "four fields");
    assert_eq!(file_nr_open("x 0 400000"), None);
    assert_eq!(file_nr_open(""), None);
}

fn put(root: &std::path::Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("its directory");
    std::fs::write(path, text).expect("the fixture");
}

/// Linux: every task against the smaller of `pid_max` and `threads-max` (as the Processes row
/// has always weighed it) and against `threads-max`, the open files against `file-max`, and the
/// caller's `RLIMIT_NPROC`; no per-user count. An unreadable file leaves its figure `None`.
#[test]
fn linux_reads_its_task_room_from_proc() {
    let root = aterm_tempfile::tempdir().expect("scratch root");
    put(root.path(), "proc/loadavg", "1.50 1.20 0.90 3/897 4242\n");
    put(root.path(), "proc/sys/kernel/pid_max", "4194304\n");
    put(root.path(), "proc/sys/kernel/threads-max", "126507\n");
    put(
        root.path(),
        "proc/sys/fs/file-nr",
        "5216\t0\t9223372036854775807\n",
    );
    put(root.path(), "proc/sys/fs/file-max", "9223372036854775807\n");
    assert_eq!(
        crate::linux::Root::new(root.path()).task_room(Some(63_253)),
        TaskRoom {
            procs: Some(897),
            procs_max: Some(126_507),
            user_procs: None,
            user_procs_max: Some(63_253),
            threads: Some(897),
            threads_max: Some(126_507),
            files: Some(5216),
            files_max: Some(9_223_372_036_854_775_807),
        }
    );
    let empty = aterm_tempfile::tempdir().expect("scratch root");
    assert_eq!(
        crate::linux::Root::new(empty.path()).task_room(None),
        TaskRoom::default()
    );
}

/// Linux: `/proc/stat`'s `processes`, past the per-core and interrupt lines, from the one read
/// that gives the core ticks; a read cut inside that line counts nothing, and a read that fails
/// forgets the last one's count.
#[test]
fn linux_reads_its_forks_from_proc_stat() {
    let stat = "cpu  10 0 5 100 0 0 0 0 0 0\ncpu0 10 0 5 100 0 0 0 0 0 0\n\
                intr 42 0 0 1 2 3\nctxt 991\nbtime 1730000000\nprocesses 31337\n\
                procs_running 2\nprocs_blocked 0\n";
    assert_eq!(crate::linux::proc_stat_forks(stat), Some(31_337));
    let cut = &stat[..stat.find("31337").expect("the count") + 3];
    assert_eq!(crate::linux::proc_stat_forks(cut), None, "a torn line");
    assert_eq!(crate::linux::proc_stat_forks("processes 12 13\n"), None);
    assert_eq!(crate::linux::proc_stat_forks("cpu 1 2 3 4\n"), None);
    let root = aterm_tempfile::tempdir().expect("scratch root");
    put(root.path(), "proc/stat", stat);
    let mut reader = crate::linux::StatReader::new(root.path(), 1);
    assert_eq!(reader.forks(), None, "nothing read yet");
    assert_eq!(reader.sample().map(|c| c.len()), Some(1), "the core ticks");
    assert_eq!(reader.forks(), Some(31_337), "the same read's forks");
    std::fs::remove_file(root.path().join("proc/stat")).expect("the fixture goes");
    assert_eq!(reader.sample(), None);
    assert_eq!(reader.forks(), None, "a failed read keeps no stale count");
}

/// Live, macOS: every pair reads, each count under its limit, this user's processes among the
/// machine's, and the threads are the processor set's threads, not its tasks (macOS runs
/// several threads a process; the set's task count is about the process count).
#[cfg(target_os = "macos")]
#[test]
fn this_macs_task_room_reads_every_pair_under_its_limit() {
    // SAFETY: `getuid` cannot fail.
    let uid = unsafe { libc::getuid() };
    let room = task_room(uid);
    let pair = |n: Option<u64>, max: Option<u64>| {
        let (n, max) = (n.expect("a count"), max.expect("a limit"));
        assert!(0 < n && n <= max, "{n} of {max}: {room:?}");
        (n, max)
    };
    let (procs, procs_max) = pair(room.procs, room.procs_max);
    let (user, user_max) = pair(room.user_procs, room.user_procs_max);
    let (threads, _) = pair(room.threads, room.threads_max);
    pair(room.files, room.files_max);
    assert!(user <= procs && user_max <= procs_max, "{room:?}");
    assert!(
        threads > procs + procs / 2,
        "{threads} threads for {procs} processes reads as the set's task count: {room:?}"
    );
    let sysctl =
        |name: &std::ffi::CStr| crate::macos::sysctl_int(name).and_then(|v| u64::try_from(v).ok());
    assert_eq!(room.procs_max, sysctl(c"kern.maxproc"));
    assert_eq!(room.threads_max, sysctl(c"kern.num_threads"));
    assert_eq!(room.files_max, sysctl(c"kern.maxfiles"));
}
