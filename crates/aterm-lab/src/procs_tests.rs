// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The same command vector works outside a jailed parent: its cwd and literal values are
/// preserved, and a variable supplied only to its parent is absent after `env -i`.
#[cfg(target_os = "macos")]
#[test]
fn a_jailed_argv_preserves_literal_values_and_chooses_its_own_directory() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("cwd with spaces ; $literal");
    std::fs::create_dir(&dir).unwrap();
    let dir = std::fs::canonicalize(dir).unwrap();
    let run = crate::run::Run {
        run: root.path().to_path_buf(),
        ..Default::default()
    };
    std::fs::write(run.run.join("jail.sb"), "(version 1)(allow default)").unwrap();
    let literal = r#"space ; $(printf expanded) `printf expanded` "$HOME" \ end"#;
    let argument = "argument with spaces ; $HOME $(printf expanded)";
    let words = jailed_argv(
        &run,
        Some(&dir),
        &[("ATERM_JAIL_LITERAL", literal)],
        &[
            "/bin/sh",
            "-c",
            "printf '%s\\n' \"$ATERM_JAIL_LITERAL\" \"$1\" \"${ATERM_JAIL_PARENT_ONLY-unset}\"; /bin/pwd -P",
            "jail-test",
            argument,
        ],
    );
    let mut cmd = aterm_uds::spawnfd::command(&words[0]);
    cmd.args(&words[1..])
        .env("ATERM_JAIL_PARENT_ONLY", "must not reach the program")
        .current_dir(root.path());
    let out = aterm_uds::deadline::output_until(
        &mut cmd,
        std::time::Instant::now() + Duration::from_secs(60),
    )
    .expect("the synthetic jail answers within a minute");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("{literal}\n{argument}\nunset\n{}\n", dir.display())
    );
}

/// A child whose launcher already chose a relative cwd must not apply that path twice.
/// CONTROL: also binding the same relative path with `env -C` cannot start the program.
#[cfg(target_os = "macos")]
#[test]
fn a_jailed_argv_leaves_an_existing_relative_working_directory_alone() {
    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let parent = cwd.join("target.noindex/.ubd");
    std::fs::create_dir_all(&parent).unwrap();
    let root = aterm_tempfile::Builder::new()
        .prefix("relative-jail-")
        .suffix(".noindex")
        .tempdir_in(&parent)
        .unwrap();
    let absolute = std::fs::canonicalize(root.path()).unwrap();
    let relative = absolute.strip_prefix(&cwd).unwrap();
    let run = crate::run::Run {
        run: absolute.clone(),
        ..Default::default()
    };
    std::fs::write(run.run.join("jail.sb"), "(version 1)(allow default)").unwrap();
    for binding in [None, Some(relative)] {
        let words = jailed_argv(&run, binding, &[], &["/bin/pwd", "-P"]);
        let mut cmd = aterm_uds::spawnfd::command(&words[0]);
        cmd.args(&words[1..]).current_dir(relative);
        let out = aterm_uds::deadline::output_until(
            &mut cmd,
            std::time::Instant::now() + Duration::from_secs(60),
        )
        .expect("the relative-cwd control answers within a minute");
        if binding.is_none() {
            assert!(out.status.success(), "{out:?}");
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                format!("{}\n", absolute.display())
            );
        } else {
            assert!(
                !out.status.success(),
                "applying the relative cwd twice must fail"
            );
        }
    }
}

/// What `/bin/ps -o <column> -p <pid>` prints, trimmed: the oracle the in-process readers
/// are held to, waited for as a one-minute hang detector.
fn ps(column: &str, pid: i32) -> String {
    let mut cmd = aterm_uds::spawnfd::command("/bin/ps");
    cmd.args(["-ww", "-o", column, "-p", &pid.to_string()]);
    let out = aterm_uds::deadline::output_until(
        &mut cmd,
        std::time::Instant::now() + Duration::from_secs(60),
    )
    .expect("/bin/ps answers within a minute");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A sleeping child of this process in a session of its own (no controlling terminal, so
/// its terminal reads `??` with no device name looked up), ended and reaped when dropped.
struct Sleeper(i32);

impl Sleeper {
    fn start() -> Self {
        // Owned before the wait, so its hang detector's panic ends it too.
        let sleep = Self(
            spawn_kept(
                aterm_uds::spawnfd::command("/bin/sleep")
                    .arg("600")
                    .setsid(true),
            )
            .expect("spawn a sleep"),
        );
        let bound = std::time::Instant::now() + Duration::from_secs(60);
        while aterm_uds::process::thread_ran(sleep.0) != Some(true) {
            assert!(std::time::Instant::now() < bound, "the sleep never ran");
            std::thread::sleep(Duration::from_millis(5));
        }
        sleep
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = crate::sys::kill(self.0, libc::SIGKILL);
        let _ = wait_gone(self.0, Duration::from_secs(60));
    }
}

/// One read of `read` once `KERN_PROC_ALL` answers: `kproc::records` bounds each wait to 1 s
/// so an `exec` in progress never stalls a sweep, while a sibling test's `exec` under load can
/// hold the kernel table past 1 s and make `read` return empty. Once a non-empty snapshot
/// arrives, the caller judges that one snapshot with no retry.
fn table_read<T>(read: fn() -> Vec<T>) -> Vec<T> {
    let bound = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let rows = read();
        if !rows.is_empty() {
            return rows;
        }
        assert!(
            std::time::Instant::now() < bound,
            "the kernel held KERN_PROC_ALL past a minute"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The table, a command line, a terminal, a `comm` and a resident set read in this process
/// say what `/bin/ps` says of the same live process.
#[test]
fn the_in_process_readers_say_what_ps_says() {
    let me = i32::try_from(std::process::id()).expect("own pid");
    let parent = i32::try_from(std::os::unix::process::parent_id()).expect("parent pid");
    assert!(
        table_read(table).contains(&(me, parent)),
        "this process and its parent"
    );
    let sleep = Sleeper::start();
    assert!(
        table_read(table).contains(&(sleep.0, me)),
        "the child, under this process"
    );
    assert_eq!(command_line(sleep.0), "/bin/sleep 600");
    assert_eq!(command_line(sleep.0), ps("command=", sleep.0));
    assert_eq!(command_line(me), ps("command=", me));
    assert!(
        table_read(commands).contains(&(sleep.0, me, "/bin/sleep 600".to_string())),
        "one read of every process names the child"
    );
    assert_eq!(tty_of(sleep.0), "??", "a child in a session of its own");
    assert_eq!(tty_of(sleep.0), ps("tty=", sleep.0));
    assert_eq!(
        rows_of(&[sleep.0, -1]),
        [(sleep.0, me, ps("comm=", sleep.0))],
        "a pid that is no process is left out"
    );
    let kib = || crate::sys::resident_size(sleep.0).expect("resident size") / 1024;
    let before = kib();
    let rss = ps("rss=", sleep.0)
        .parse::<u64>()
        .expect("ps prints a resident set");
    let after = kib();
    assert!(
        before.min(after) <= rss && rss <= before.max(after),
        "ps's resident set ({rss} KiB) lies between two read in this process ({before}, {after})"
    );
}

/// A start token names one process: the same for two reads of it, another for another
/// process, and empty for pid 1, for no pid, and once the process is gone.
#[test]
fn a_start_names_one_process_and_none_once_it_is_gone() {
    let me = i32::try_from(std::process::id()).expect("own pid");
    let sleep = Sleeper::start();
    let start = start_time(sleep.0);
    assert!(!start.is_empty(), "the child's start reads");
    assert_eq!(start_time(sleep.0), start);
    assert_ne!(start_time(me), start);
    assert_eq!(start_time(1), "");
    assert_eq!(start_time(0), "");
    let pid = sleep.0;
    drop(sleep);
    assert_eq!(start_time(pid), "", "a reaped child has no start");
}

/// A child that exited and is not yet reaped reads as a zombie, `<defunct>` with nothing
/// read; once reaped it is neither a zombie nor anything else.
#[test]
fn an_unreaped_child_is_a_zombie_until_it_is_reaped() {
    let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
        .spawn()
        .expect("spawn true");
    let pid = i32::try_from(child.id()).expect("pid");
    let bound = std::time::Instant::now() + Duration::from_secs(60);
    while !zombie(pid) {
        assert!(std::time::Instant::now() < bound, "true never exited");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(command_line(pid), aterm_sysprobe::kproc::DEFUNCT);
    child.wait().expect("reap it");
    assert!(!zombie(pid));
    assert_eq!(command_line(pid), "");
}

/// A child whose one thread has not run (started suspended, as macOS's program check holds
/// one) keeps its arguments unread: an empty command line and `comm`, and none in the table
/// of every process, which still lists it.
#[test]
fn a_process_whose_thread_has_not_run_has_its_arguments_left_unread() {
    let me = i32::try_from(std::process::id()).expect("own pid");
    let held =
        aterm_uds::deadline::Held::start(aterm_uds::spawnfd::command("/bin/sleep").arg("600"))
            .expect("a start-suspended sleep");
    let pid = i32::try_from(held.id()).expect("pid");
    assert_eq!(aterm_uds::process::thread_ran(pid), Some(false));
    assert_eq!(command_line(pid), "");
    assert!(table_read(commands).contains(&(pid, me, String::new())));
    assert_eq!(rows_of(&[pid]), [(pid, me, String::new())]);
    drop(held);
}

#[test]
fn descendants_come_children_first_and_never_for_init() {
    let t = [(10, 1), (11, 10), (12, 11), (13, 10), (20, 1)];
    assert_eq!(descendants_in(&t, 10), vec![12, 11, 13]);
    assert!(descendants_in(&t, 1).is_empty());
    assert!(descendants_in(&t, 0).is_empty());
    let deep: Vec<(i32, i32)> = (11..=90).map(|pid| (pid, pid - 1)).collect();
    let got = descendants_in(&deep, 10);
    let want: Vec<i32> = (11..=90).rev().collect();
    assert_eq!(
        got, want,
        "deep process chains past 64 levels are not truncated"
    );
    let cyclic = [(11, 10), (12, 11), (10, 12), (11, 12)];
    assert_eq!(descendants_in(&cyclic, 10), vec![12, 11]);
}

#[test]
fn an_ended_child_is_reaped_and_reads_dead() {
    let pid = spawn_kept(
        aterm_uds::spawnfd::command("/usr/bin/true").stdout(aterm_uds::spawnfd::Stdio::null()),
    )
    .unwrap();
    assert!(wait_gone(pid, Duration::from_secs(60)), "true exits");
    assert!(!alive(pid));
}

/// A child the kernel would not watch, killed and on its way out, is
/// waited for until it is reaped ([`ended_within`]): one read of `alive`
/// right after the SIGKILL can still find it running.
#[test]
fn a_killed_child_the_kernel_will_not_watch_is_waited_for_until_reaped() {
    let pid = spawn_kept(aterm_uds::spawnfd::command("/bin/sleep").arg("600")).unwrap();
    assert!(
        !ended_within(pid, Duration::from_millis(50)),
        "sleep 600 lives"
    );
    assert!(alive(pid), "still ours to reap");
    crate::sys::kill(pid, libc::SIGKILL).unwrap();
    assert!(ended_within(pid, Duration::from_secs(60)), "reaped");
    assert!(!crate::sys::alive(pid), "no zombie keeps the pid");
    let me = i32::try_from(std::process::id()).unwrap();
    assert!(
        !ended_within(me, Duration::from_secs(60)),
        "not ours: read once"
    );
}

/// A jailed child in a session of its own kept by [`keep_jailed`] (the lab's broker) is one
/// the sweeps see when the sandbox gives no answer: [`jail_members`] of a run with no mark
/// takes only the process tree, and no pid the run records is the child's parent.
#[test]
fn a_kept_jailed_child_is_swept_without_a_sandbox_answer() {
    let root = aterm_tempfile::tempdir().unwrap();
    let run = crate::run::Run {
        run: root.path().to_path_buf(),
        ..crate::run::Run::default()
    };
    #[expect(
        clippy::disallowed_types,
        reason = "the shape of the lab's jailed launch: a std Child in a session of its own"
    )]
    let mut cmd = std::process::Command::new("/bin/sleep");
    cmd.arg("600")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::sys::in_own_session(&mut cmd);
    let pid = keep_jailed(&run.run, cmd.spawn().unwrap());
    let seen = jail_members(&run);
    let roots = jailed_roots(&run.run, false);
    crate::sys::kill(pid, libc::SIGKILL).unwrap();
    assert!(ended_within(pid, Duration::from_secs(60)), "reaped");
    assert_eq!(roots, vec![pid], "recorded as a jailed root");
    assert!(seen.contains(&pid), "swept: {seen:?} lacks {pid}");
    assert!(
        !jail_members(&run).contains(&pid),
        "an ended root is no member"
    );
}

/// A witness whose thread has not run is waited for, not read as empty and so missing: with
/// no time left it is not seen, a held process the kernel names otherwise is not waited on,
/// and a wait that lets it run (here, its first pause resumes it) sees it once it runs.
#[test]
fn a_witness_still_held_at_its_exec_is_waited_for() {
    let line = ("sleep", "/bin/sleep 600");
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .start_suspended(true)
        .spawn()
        .expect("a start-suspended sleep");
    let pid = i32::try_from(child.id()).unwrap_or(0);
    let unwaited = runs_with(pid, line, Duration::ZERO, || {});
    let mut paused = false;
    let other = runs_with(pid, ("aterm-lab", line.1), Duration::from_secs(60), || {
        paused = true;
    });
    let mut resumed = None;
    let waited = runs_with(pid, line, Duration::from_secs(60), || {
        if resumed.is_none() {
            resumed = Some(child.resume().is_ok());
        }
        std::thread::sleep(Duration::from_millis(5));
    });
    // Ended and reaped before any verdict: a held child left behind keeps this test's
    // output pipe open.
    let _ = child.kill();
    let _ = child.wait();
    assert!(!unwaited, "unread while held");
    assert!(
        !other && !paused,
        "another program's held pid is read at once"
    );
    assert_eq!(resumed, Some(true), "it was held when the wait began");
    assert!(waited, "seen once it runs");
}

/// A borrowed PID is not this run's root merely because it lives. Only the exact canonical
/// start qualifies; a different current start, an unread record, legacy text and malformed
/// tokens remain visible in historical evidence without admitting them or their children.
#[test]
fn jailed_signal_candidates_require_their_own_canonical_start() {
    let records = [
        "70\tSat Nov  1 10:23:45 2026",
        "72\t1793550000.12345",
        "74\t1793550000.123456",
        "76\t1793550000.123456",
        "78\t",
        "80\t1793550001.654321",
        "82",
        "1\t1793550001.654321",
    ];
    let text = records.join("\n");
    let mut read = Vec::new();
    let roots = jailed_roots_with(&text, false, |pid| {
        read.push(pid);
        match pid {
            74 => "1793551000.123456".into(), // Same PID, another process.
            80 => "1793550001.654321".into(),
            _ => String::new(),
        }
    });
    assert_eq!(roots, [80], "only the exact current process is admitted");
    assert_eq!(read, [74, 76, 80], "unproven formats grant no PID lookup");
    let table = [(70, 1), (71, 70), (72, 1), (73, 72), (80, 1), (81, 80)];
    let mut candidates = roots.clone();
    for root in roots {
        candidates.extend(descendants_in(&table, root));
    }
    assert_eq!(
        candidates,
        [80, 81],
        "no borrowed root or child is selected"
    );
    assert_eq!(
        jailed_roots_with(&text, true, |_| panic!("history needs no process proof")),
        [70, 72, 74, 76, 78, 80, 82],
        "all stored evidence stays available without becoming signal authority"
    );
}

/// AN EXIT THE WATCH SAW IS WAITED ON UNTIL THE CHILD IS REAPED
/// ([`settled`]): told its exit came while the child still runs (as the
/// kernel's event can come before the child is a zombie its parent can
/// reap), the wait reaps it once it ends, so it never reads alive after
/// the wait said it ended.
#[test]
fn an_exit_the_watch_saw_is_waited_on_until_the_child_is_reaped() {
    let long = spawn_kept(aterm_uds::spawnfd::command("/bin/sleep").arg("600")).unwrap();
    assert!(!settled(long, false, Duration::from_secs(60)), "it runs");
    crate::sys::kill(long, libc::SIGKILL).unwrap();
    let pid = spawn_kept(aterm_uds::spawnfd::command("/bin/sleep").arg("0.3")).unwrap();
    assert!(settled(pid, true, Duration::from_secs(60)), "its exit seen");
    assert!(!crate::sys::alive(pid), "reaped: no zombie keeps the pid");
    assert!(!alive(pid));
    assert!(settled(long, true, Duration::from_secs(60)), "killed");
    assert!(!crate::sys::alive(long), "reaped");
    let me = i32::try_from(std::process::id()).unwrap();
    assert!(
        settled(me, true, Duration::from_secs(60)),
        "not ours: read once"
    );
}

/// A child's paths ([`open_paths`]) are its working directory, its executable and each file
/// it holds through a descriptor, in descriptor order, a PTY master as `/dev/ptmx`; its table
/// counts those descriptors ([`crate::sys::fd_count`]). Past its stdin, stdout and stderr the
/// child may hold files it opened itself (dyld opens `/dev/dtracehelper` while it starts).
#[test]
fn a_childs_paths_and_descriptors_are_read_in_process() {
    let dir = crate::TestScratch::new("procs");
    let held = dir.join("held");
    let master = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/ptmx")
        .expect("a PTY master");
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .current_dir(&*dir)
        .stdin(master)
        .stdout(std::fs::File::create(&held).unwrap())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn a holder");
    let pid = i32::try_from(child.id()).unwrap();
    let (paths, count) = (open_paths(pid), crate::sys::fd_count(pid));
    let _ = child.kill();
    let _ = child.wait();
    let want: Vec<PathBuf> = [
        dir.as_path(),
        Path::new("/bin/sleep"),
        Path::new("/dev/ptmx"),
        held.as_path(),
        Path::new("/dev/null"),
    ]
    .map(Path::to_path_buf)
    .into();
    let paths = paths.expect("its paths");
    assert!(paths.complete);
    assert_eq!(
        paths.paths.get(..want.len()),
        Some(&want[..]),
        "{:?}",
        paths.paths
    );
    assert!(
        count.is_some_and(|count| count >= 3),
        "stdin, stdout and stderr are counted; the child can open more"
    );
}

#[test]
fn only_complete_process_paths_can_report_an_absence() {
    use crate::codex_proof::Verdict;
    use aterm_uds::holders::Exe;
    use std::io::ErrorKind;

    let home = ["/Users//owner".to_owned()];
    let report = |read| {
        let seen = crate::selftest::open_files_report(7, read);
        crate::selftest::strays_verdict(
            &crate::selftest::owner_strays(&seen.text, &home, &[]),
            &[7],
            seen.complete,
        )
    };
    let read = |file: &str, cwd: Option<&str>, exe| {
        open_paths_with(
            7,
            |_| Ok(vec![PathBuf::from(file)]),
            |_| cwd.map(PathBuf::from),
            |_| exe,
        )
    };
    let scratch = "/private/tmp/owned";
    let executable = || Exe::At(PathBuf::from("/bin/sleep"));
    let complete = read(scratch, Some(scratch), executable()).expect("all paths read");
    assert!(complete.complete);
    assert_eq!(
        complete.paths,
        [
            PathBuf::from(scratch),
            PathBuf::from("/bin/sleep"),
            PathBuf::from(scratch)
        ]
    );
    assert_eq!(report(Ok(complete)), Verdict::Ok);
    for (file, cwd, exe) in [
        ("/Users//owner/file", scratch, executable()),
        (scratch, "/Users//owner", executable()),
        (
            scratch,
            scratch,
            Exe::At(PathBuf::from("/Users//owner/program")),
        ),
    ] {
        assert_eq!(report(read(file, Some(cwd), exe)), Verdict::Fail);
    }
    for file in [scratch, "/Users//owner/file"] {
        for (cwd, exe) in [(None, executable()), (Some(scratch), Exe::Unnamed)] {
            let partial = read(file, cwd, exe).expect("the descriptors read");
            assert!(!partial.complete);
            assert!(partial.paths.contains(&PathBuf::from(file)));
            assert_eq!(
                report(Ok(partial)),
                if file == scratch {
                    Verdict::Unproven
                } else {
                    Verdict::Fail
                }
            );
        }
    }
    let exited = read(scratch, Some(scratch), Exe::Exited)
        .expect_err("the executable read found an exited process");
    assert_eq!(exited.kind(), ErrorKind::NotFound);
    assert_eq!(report(Err(exited)), Verdict::Unproven);
    for kind in [
        ErrorKind::NotFound,
        ErrorKind::PermissionDenied,
        ErrorKind::Other,
    ] {
        let error = open_paths_with(
            7,
            |_| Err(std::io::Error::new(kind, "descriptor read failed")),
            |_| panic!("no cwd read after the descriptor error"),
            |_| panic!("no executable read after the descriptor error"),
        )
        .expect_err("the descriptor error is preserved");
        assert_eq!(error.kind(), kind);
        assert_eq!(error.to_string(), "descriptor read failed");
        assert_eq!(report(Err(error)), Verdict::Unproven);
    }
    assert_eq!(
        open_paths(-1)
            .expect_err("invalid pid before any read")
            .kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn a_wait_that_runs_out_leaves_the_child_to_a_later_one() {
    let pid = spawn_kept(aterm_uds::spawnfd::command("/bin/sleep").arg("600")).unwrap();
    assert!(
        !wait_gone(pid, Duration::from_millis(50)),
        "sleep 600 lives"
    );
    assert!(alive(pid), "still ours to reap");
    crate::sys::kill(pid, libc::SIGKILL).unwrap();
    assert!(wait_gone(pid, Duration::from_secs(60)), "a SIGKILL ends it");
    assert!(!alive(pid));
    assert!(!crate::sys::alive(pid), "reaped: no zombie keeps the pid");
}
