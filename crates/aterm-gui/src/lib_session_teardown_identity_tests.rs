// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::{Duration, Instant};

/// A sleep in a session of its own, started with the signals that end a process at their
/// defaults, set here, not this test binary's: one started ignoring SIGHUP or SIGTERM (a `nohup`
/// or `&` runner's choice, which every exec keeps) would outlive a stray hang-up or this test's
/// own SIGTERM.
fn setsid_sleep() -> aterm_uds::spawnfd::Child {
    let start = aterm_spec::start::at_defaults(&aterm_spec::start::TERMINATING);
    let sleep = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .setsid(true)
        .spawn()
        .expect("the bystander sleep must spawn");
    drop(start);
    sleep
}

fn running(child: &mut aterm_uds::spawnfd::Child) -> bool {
    matches!(child.try_wait(), Ok(None))
}

#[test]
fn dropping_a_session_whose_pid_names_another_process_signals_nothing() {
    let mut bystander = setsid_sleep();
    let pid = i32::try_from(bystander.id()).expect("pid fits i32");
    // SAFETY: read-only identity probe of the test's own child.
    assert_eq!(
        unsafe { libc::getpgid(pid) },
        pid,
        "NON-VACUITY: the bystander passes the old guard"
    );
    let born = aterm_pty::process_birth(pid).expect("a live child reads a birth");
    let mut session = stub_session(1);
    session.pid = pid;
    session.shell_identity = aterm_pty::ShellIdentity::from_parts(pid, Some(born - 1), false);
    assert!(
        !session.shell_identity.verify(),
        "the stale identity must not verify"
    );
    drop(session);
    assert!(
        aterm_pty::wait_for_reaps(aterm_spec::wait::MUST_HAPPEN),
        "the teardown's reaps never settled"
    );
    // SAFETY: a signal to the test's own child, not yet reaped.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let status = bystander
        .wait()
        .expect("the bystander is the test's to reap: the teardown reaped it");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(libc::SIGTERM),
        "closing the tab SIGNALLED an unrelated process that holds its shell's old pid: {status}"
    );
}

#[test]
fn dropping_a_session_whose_shell_still_verifies_hangs_it_up() {
    let mut shell = setsid_sleep();
    let pid = i32::try_from(shell.id()).expect("pid fits i32");
    let mut session = stub_session(1);
    session.pid = pid;
    session.shell_identity = aterm_pty::record_spawned_shell(pid);
    assert!(
        session.shell_identity.verify(),
        "PRECONDITION: our live child verifies"
    );
    drop(session);
    let deadline = Instant::now() + Duration::from_secs(60);
    while running(&mut shell) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let hung_up = !running(&mut shell);
    let _ = shell.kill();
    let _ = shell.wait();
    assert!(
        hung_up,
        "a verified shell must still be hung up when its tab closes (keeper F3)"
    );
}

/// An adopted shell belongs to the predecessor until Commit: dropping its session before Commit leaves it alive, while dropping after Commit hangs it up.
#[test]
fn dropping_an_adopted_session_before_commit_leaves_its_shell_alive() {
    for committed in [false, true] {
        let mut shell = setsid_sleep();
        let pid = i32::try_from(shell.id()).expect("pid fits i32");
        let commit = Arc::new(crate::shell_rekey::Deferred::holding());
        if committed {
            commit.release();
        }
        let mut session = stub_session(1);
        session.pid = pid;
        session.handoff_local_id = Some(7);
        session.shell_identity = aterm_pty::record_spawned_shell(pid);
        session.handoff_commit = Some(Arc::clone(&commit));
        assert!(session.shell_identity.verify(), "PRECONDITION");
        drop(session);
        if committed {
            let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while running(&mut shell) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let hung_up = !running(&mut shell);
            let _ = shell.kill();
            let _ = shell.wait();
            assert!(
                hung_up,
                "committed = true: a committed session's shell is hung up"
            );
        } else {
            assert!(
                aterm_pty::wait_for_reaps(aterm_spec::wait::MUST_HAPPEN),
                "the teardown's reaps never settled"
            );
            // SAFETY: signals the test's own child, not yet reaped.
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let status = shell
                .wait()
                .expect("the predecessor's shell is the test's to reap");
            assert_eq!(
                std::os::unix::process::ExitStatusExt::signal(&status),
                Some(libc::SIGTERM),
                "committed = false: the predecessor's shell survives an uncommitted drop: {status}"
            );
        }
    }
}

/// Dropping an adopted session before Commit preserves the predecessor's rekey, body, and trap files while withdrawing uncommitted writes.
#[test]
fn dropping_an_adopted_session_before_commit_leaves_the_predecessors_rekey_and_body_files() {
    let Some(_env) = crate::seamless::tests::env_alone() else {
        return;
    };
    let _scratch = crate::seamless::tests::ScratchEnv::runtime("predecessor-files");
    for committed in [false, true] {
        let commit = Arc::new(crate::shell_rekey::Deferred::holding());
        let mut session = stub_session(1);
        session.handoff_local_id = Some(7);
        session.rekey_channel = true;
        session.body_loader = true;
        session.handoff_commit = Some(Arc::clone(&commit));
        let sid = session.ctx.self_id.clone();
        let rekey = crate::shell_rekey::path_for(&sid).expect("a server-minted sid");
        let body = crate::shell_body::path_for(&sid).expect("a server-minted sid");
        let mut trap = rekey.clone().into_os_string();
        trap.push(aterm_core::shell_integration::DEBUG_TRAP_SUFFIX);
        let trap = std::path::PathBuf::from(trap);
        std::fs::write(&rekey, "parent-key\n").expect("the parent's re-key");
        std::fs::write(&body, "parent-body\n").expect("the parent's body pointer");
        std::fs::write(&trap, "trap -- ': rc' DEBUG\n").expect("the shell's trap read");
        commit.hold(crate::shell_rekey::Pending::body(
            &body,
            "successor-body",
            7,
        ));
        if committed {
            commit.release();
            assert_eq!(
                std::fs::read_to_string(&body).ok().as_deref(),
                Some("successor-body\n"),
                "PRECONDITION: the Commit wrote this process's pointer"
            );
        }
        drop(session);
        if committed {
            assert!(
                !rekey.exists() && !body.exists() && !trap.exists(),
                "a committed session's untaken files, and its shell's trap read, go with it"
            );
            continue;
        }
        assert_eq!(
            (
                std::fs::read_to_string(&rekey).ok(),
                std::fs::read_to_string(&body).ok(),
                trap.exists()
            ),
            (
                Some("parent-key\n".to_string()),
                Some("parent-body\n".to_string()),
                true
            ),
            "an uncommitted drop deleted the files the predecessor handed its shell"
        );
        commit.release();
        assert_eq!(
            std::fs::read_to_string(&body).ok().as_deref(),
            Some("parent-body\n"),
            "a write held for a dropped session must not land"
        );
        let _ = std::fs::remove_file(&rekey);
        let _ = std::fs::remove_file(&body);
        let _ = std::fs::remove_file(&trap);
    }
}

/// `spawn::adopted_handoff_commit` preserves only an uncommitted, non-recovered adopted shell when dropped.
#[test]
fn a_session_built_through_the_spawn_s_latch_keeps_only_an_uncommitted_adopted_shell() {
    let overlap = Arc::new(crate::shell_rekey::Deferred::holding());
    for (adopted, recovered, factory_latch, survives) in [
        (true, false, Some(&overlap), true),
        (false, false, Some(&overlap), false),
        (true, true, Some(&overlap), false),
        (true, false, None, false),
    ] {
        let mut shell = setsid_sleep();
        let pid = i32::try_from(shell.id()).expect("pid fits i32");
        let mut session = stub_session(1);
        session.pid = pid;
        session.handoff_local_id = adopted.then_some(7);
        session.shell_identity = aterm_pty::record_spawned_shell(pid);
        session.handoff_commit =
            crate::spawn::adopted_handoff_commit(adopted, recovered, factory_latch);
        assert!(session.shell_identity.verify(), "PRECONDITION");
        drop(session);
        if survives {
            assert!(
                aterm_pty::wait_for_reaps(aterm_spec::wait::MUST_HAPPEN),
                "the teardown's reaps never settled"
            );
            // SAFETY: signals the test's own child, not yet reaped.
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let status = shell
                .wait()
                .expect("the surviving adopted shell is the test's to reap");
            assert_eq!(
                std::os::unix::process::ExitStatusExt::signal(&status),
                Some(libc::SIGTERM),
                "adopted = {adopted}, recovered = {recovered}, overlap = {}: {status}",
                factory_latch.is_some()
            );
        } else {
            let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while running(&mut shell) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let hung_up = !running(&mut shell);
            let _ = shell.kill();
            let _ = shell.wait();
            assert!(
                hung_up,
                "adopted = {adopted}, recovered = {recovered}, overlap = {}",
                factory_latch.is_some()
            );
        }
    }
    assert!(!overlap.released(), "PRECONDITION: no Commit in this test");
}

/// `SessionPool::retire_at_quit` terminates a `SIGHUP`-ignoring shell while leaving an uncommitted adopted shell alive.
#[cfg(unix)]
#[test]
fn the_final_exit_ends_a_hup_ignoring_shell_as_closing_its_tab_does() {
    let mut holdout = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "trap '' HUP; exec /bin/sleep 600"])
        .setsid(true)
        .spawn()
        .expect("the HUP-ignoring shell must spawn");
    let mut handed = setsid_sleep();
    let mut pool = SessionPool::default();
    let mut tab = stub_session(1);
    tab.pid = i32::try_from(holdout.id()).expect("pid fits i32");
    tab.shell_identity = aterm_pty::record_spawned_shell(tab.pid);
    pool.insert(tab);
    let mut adopted = stub_session(2);
    adopted.pid = i32::try_from(handed.id()).expect("pid fits i32");
    adopted.shell_identity = aterm_pty::record_spawned_shell(adopted.pid);
    adopted.handoff_local_id = Some(7);
    adopted.handoff_commit = Some(Arc::new(crate::shell_rekey::Deferred::holding()));
    pool.insert(adopted);

    pool.retire_at_quit(&[]);
    let settled = aterm_pty::wait_for_reaps(Duration::from_secs(60));
    let deadline = Instant::now() + Duration::from_secs(60);
    while running(&mut holdout) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let ended = !running(&mut holdout);
    let kept = running(&mut handed);
    for child in [&mut holdout, &mut handed] {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(pool);
    assert!(settled, "the reaps never settled");
    assert!(
        ended,
        "the final exit left a HUP-ignoring shell running that closing its tab ends"
    );
    assert!(
        kept,
        "the final exit hung up a shell the predecessor still owns"
    );
}

/// A session this process left to the keeper (`Session::leave_to_keeper`, the Mac Setup tab's
/// installer a quit carries) outlives the final exit's hang-up and its own drop.
///
/// NEGATIVE CONTROL: its pool sibling, not left to the keeper, is hung up.
#[cfg(unix)]
#[test]
fn a_session_left_to_the_keeper_outlives_the_final_exit() {
    let mut installer = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .setsid(true)
        .spawn()
        .expect("the installer stand-in must spawn");
    let mut shell = setsid_sleep();
    let mut pool = SessionPool::default();
    let mut carried = stub_session(1);
    carried.pid = i32::try_from(installer.id()).expect("pid fits i32");
    carried.shell_identity = aterm_pty::record_spawned_shell(carried.pid);
    pool.insert(carried);
    let mut sibling = stub_session(2);
    sibling.pid = i32::try_from(shell.id()).expect("pid fits i32");
    sibling.shell_identity = aterm_pty::record_spawned_shell(sibling.pid);
    pool.insert(sibling);
    pool.get(1).expect("the carried session").leave_to_keeper();

    pool.retire_at_quit(&[1]);
    drop(pool);
    let settled = aterm_pty::wait_for_reaps(aterm_spec::wait::MUST_HAPPEN);
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while running(&mut shell) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let hung_up = !running(&mut shell);
    let kept = running(&mut installer);
    for child in [&mut installer, &mut shell] {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(settled, "the reaps never settled");
    assert!(
        hung_up,
        "CONTROL: the final exit hangs up a session it does not leave to the keeper"
    );
    assert!(
        kept,
        "the final exit hung up the installer it left to the keeper"
    );
}

/// `App::prepare_cold_exec` waits for pending tab reaps to settle before executing the cold-update image.
#[cfg(unix)]
#[test]
fn the_cold_exec_runs_only_once_a_closed_tabs_holdout_has_its_sigkill() {
    use std::io::BufRead as _;
    if !crate::test_alone::alone() {
        return;
    }
    let mut holdout = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "trap '' HUP; echo up; exec /bin/sleep 600"])
        .setsid(true)
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("the HUP-ignoring shell must spawn");
    let stdout = holdout.stdout.take().expect("the holdout's stdout");
    let (up_tx, up_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::BufReader::new(stdout).read_line(&mut line);
        let _ = up_tx.send(line);
    });
    assert_eq!(
        up_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).as_deref(),
        Ok("up\n"),
        "PRECONDITION: the holdout's SIGHUP trap is set"
    );
    let mut app = App::headless_for_test();
    let mut tab = stub_session(1);
    tab.pid = i32::try_from(holdout.id()).expect("pid fits i32");
    tab.shell_identity = aterm_pty::record_spawned_shell(tab.pid);
    let hold = aterm_pty::hold_reap_kills_for_test(tab.pid);
    drop(tab);
    assert!(
        !aterm_pty::wait_for_reaps(Duration::ZERO),
        "PRECONDITION: the closed tab's reap is in its grace"
    );
    assert!(
        running(&mut holdout),
        "PRECONDITION: the holdout outlived its hang-up"
    );
    drop(hold);

    app.prepare_cold_exec(7, true, false);
    let mut settled_at_exec = None;
    let returned = app.run_cold_exec(|| {
        settled_at_exec = Some(aterm_pty::wait_for_reaps(Duration::ZERO));
        std::io::Error::from(std::io::ErrorKind::NotFound)
    });
    app.resume_after_cold_exec();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while running(&mut holdout) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let ended = !running(&mut holdout);
    let _ = holdout.kill();
    let _ = holdout.wait();
    assert_eq!(
        returned.kind(),
        std::io::ErrorKind::NotFound,
        "the exec ran"
    );
    assert_eq!(
        settled_at_exec,
        Some(true),
        "the exec ran with a closed tab's HUP-ignoring shell still in its grace"
    );
    assert!(ended, "the holdout was SIGKILLed");
}
/// A pooled session over a real PTY running `script` under `/bin/bash`, its master owned by its
/// sink as a spawned tab's is, once the script has printed `READY`.
#[cfg(target_os = "macos")]
fn pty_session(id: u64, script: &str) -> Session {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    // SAFETY: the test owns its synthetic shell and trusted spawn capabilities.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let exec = vec![
        std::ffi::OsString::from("/bin/bash"),
        "-c".into(),
        script.into(),
    ];
    let child = aterm_pty::spawn_shell_with_pid(
        24,
        80,
        &cap,
        &sandbox,
        &[],
        None,
        None,
        None,
        Some(&exec),
        None,
        None,
        aterm_sandbox::Limits::inherit(),
    )
    .expect("the shell starts");
    aterm_pty::set_nonblocking(child.master, true).expect("a nonblocking master");
    // SAFETY: the new PTY master has no other owner.
    let master = unsafe { OwnedFd::from_raw_fd(child.master) };
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut seen = Vec::new();
    while !seen.windows(5).any(|word| word == b"READY") {
        assert!(Instant::now() < deadline, "the shell never printed READY");
        let mut bytes = [0; 256];
        let n = aterm_pty::read(master.as_raw_fd(), &mut bytes);
        if n > 0 {
            seen.extend_from_slice(&bytes[..n as usize]);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let raw = master.as_raw_fd();
    let mut session = stub_session_with_sink(id, Arc::new(SinkWriter::new_owned(master)));
    session.master = raw;
    session.pid = child.pid;
    session.shell_identity = aterm_pty::record_spawned_shell(child.pid);
    session
}

/// Quit retires each master it hangs up, so a hang-up handler that reads its terminal meets end
/// of file and runs to its end while its reap waits, kills held so nothing else ends it; a
/// session its predecessor still owns keeps its terminal and its shell. With the master held open
/// until exit, the handler's `cat` waited on it until the reaper's kill tick (ATERM-KI-1104-7).
#[cfg(target_os = "macos")]
#[test]
fn quitting_retires_each_master_it_hangs_up_so_a_handler_reading_its_terminal_ends() {
    if !crate::test_alone::alone() {
        return;
    }
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let done = scratch.path().join("done");
    let tab = pty_session(
        1,
        &format!(
            "trap 'cat > /dev/null; printf done > \"{}\"; exit 0' HUP; printf READY; \
             while :; do read line; done",
            done.display()
        ),
    );
    let mut adopted = pty_session(2, "printf READY; exec /bin/sleep 600");
    adopted.handoff_local_id = Some(7);
    adopted.handoff_commit = Some(Arc::new(crate::shell_rekey::Deferred::holding()));
    let (tab_pid, adopted_pid, adopted_master) = (tab.pid, adopted.pid, adopted.master);
    let hold = aterm_pty::hold_reap_kills_for_test(tab_pid);
    let mut pool = SessionPool::default();
    pool.insert(tab);
    pool.insert(adopted);

    pool.retire_at_quit(&[]);
    let ended = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(5),
        || done.exists().then_some(()),
    )
    .is_some();
    // SAFETY: a read-only probe of the descriptor the adopted session's sink owns.
    let kept_terminal = unsafe { libc::isatty(adopted_master) } == 1;
    let kept_shell = aterm_pty::child_exit_unreaped(adopted_pid) == Some(false);
    // SAFETY: ending the test's own children; the adopted one, which no teardown reaps, is reaped
    // here.
    unsafe {
        if aterm_pty::child_exit_unreaped(tab_pid) == Some(false) {
            libc::kill(tab_pid, libc::SIGKILL);
        }
        libc::kill(adopted_pid, libc::SIGKILL);
        libc::waitpid(adopted_pid, std::ptr::null_mut(), 0);
    }
    drop(hold);
    drop(pool);
    assert!(
        aterm_pty::wait_for_reaps(aterm_spec::wait::MUST_HAPPEN),
        "the reaps never settled"
    );
    assert!(
        ended,
        "the hang-up handler's `cat` never read end of file: Quit kept the master open"
    );
    assert!(
        kept_terminal && kept_shell,
        "Quit retired the master or ended the shell of a session its predecessor still owns \
         (terminal kept: {kept_terminal}, shell kept: {kept_shell})"
    );
}
