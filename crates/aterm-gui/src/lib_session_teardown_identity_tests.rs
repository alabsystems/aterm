// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::{Duration, Instant};

/// A `sleep` in its own session: a session and group leader, like the job-control leader
/// or daemon a reissued shell pid can land on, so `getpgid(pid) == pid` alone passes it.
fn setsid_sleep() -> aterm_uds::spawnfd::Child {
    aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .setsid(true)
        .spawn()
        .expect("the bystander sleep must spawn")
}

/// Whether the child is still running. `Err` is "already reaped by someone
/// else" (the teardown's reaper thread), which is not running either.
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
    // The adopted shell this session took was born a microsecond earlier:
    // it has exited, launchd reaped it, and the kernel reissued its pid.
    session.shell_identity = aterm_pty::ShellIdentity::from_parts(pid, Some(born - 1), false);
    assert!(
        !session.shell_identity.verify(),
        "the stale identity must not verify"
    );
    drop(session);
    // Every reap the drop started has settled or sent its SIGKILL (a hang
    // detector, not a budget), so any signal the teardown sends is sent.
    assert!(
        aterm_pty::wait_for_reaps(aterm_spec::wait::MUST_HAPPEN),
        "the teardown's reaps never settled"
    );
    // Then this test ends the bystander with SIGTERM. A SIGHUP or SIGKILL the
    // teardown sent ended it already or is delivered first (lowest signal
    // first), so how it died names who signalled it, however late a
    // descheduled reaper ran.
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

/// An adopted shell belongs to the predecessor until Commit: dropping its session before
/// Commit leaves it alive for the rollback to resume. Negative control: after Commit,
/// closing it hangs the shell up.
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
        let deadline = Instant::now() + Duration::from_millis(if committed { 60_000 } else { 500 });
        while running(&mut shell) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let alive = running(&mut shell);
        let _ = shell.kill();
        let _ = shell.wait();
        assert_eq!(
            alive, !committed,
            "committed = {committed}: the predecessor's shell survives an uncommitted drop, \
             and a committed session's shell is hung up"
        );
    }
}

/// …AND SO ARE THE FILES THE PREDECESSOR HANDED THAT SHELL.
/// The parent re-keyed the shell, or pointed it at its
/// body, at an earlier update's Commit; the shell (an agent tab) has not
/// reached a prompt to take either, so both still wait at
/// `<control dir>/rekey/<sid>` and `<control dir>/integration/<sid>` — paths
/// keyed by the sid alone, which the successor inherits. A drop before
/// this update's Commit deleted both, so after the rollback the resumed
/// shell kept signing with a key the parent no longer accepts (`status
/// integration=degraded`) and stayed on its older body. The drop now
/// leaves them, and withdraws only this process's own held writes for the
/// sid, so a later release cannot put its key over the parent's.
/// NEGATIVE CONTROL: after Commit the files are this process's, and the
/// drop removes them as before — with the `<sid>.trap` a bash writes beside
/// its re-key path at load (2026-10-17), which was left behind before.
///
/// RED before the fix: both files were gone after the uncommitted drop.
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
        // The predecessor's files, written at ITS Commit, and the one its bash
        // wrote at its load (its read of the rc's DEBUG trap).
        std::fs::write(&rekey, "parent-key\n").expect("the parent's re-key");
        std::fs::write(&body, "parent-body\n").expect("the parent's body pointer");
        std::fs::write(&trap, "trap -- ': rc' DEBUG\n").expect("the shell's trap read");
        // This process's own write for the same shell, held for its Commit.
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
        // Its own held write went with the session: a release leaves the predecessor's
        // pointer as it is.
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

/// THE LATCH `spawn_session` HANDS A SESSION decides that drop:
/// built through `spawn::adopted_handoff_commit` — the one
/// expression `spawn_session` sets `handoff_commit` with — over a factory whose `rekey_deferred` is an overlap handoff's
/// `Deferred::holding()`, an ADOPTED session's drop leaves its shell to the
/// predecessor. NEGATIVE CONTROLS, each hung up as before: a fresh spawn, a
/// shell the PTY keeper kept (`recovered`), and an adoption with no overlap
/// handoff (no latch).
///
/// RED against `handoff_commit: None` (or a latch that ignores `adopted`):
/// the adopted shell is dead at 500 ms (or the fresh one survives).
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
        let deadline = Instant::now() + Duration::from_millis(if survives { 500 } else { 60_000 });
        while running(&mut shell) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let alive = running(&mut shell);
        let _ = shell.kill();
        let _ = shell.wait();
        assert_eq!(
            alive,
            survives,
            "adopted = {adopted}, recovered = {recovered}, overlap = {}",
            factory_latch.is_some()
        );
    }
    assert!(!overlap.released(), "PRECONDITION: no Commit in this test");
}

/// A quit ends every tab's shell as closing the tab does. The final exit forgets the App,
/// so no session drops; `hang_up_at_exit` runs each one's hang-up and reap instead. This
/// shell ignores SIGHUP, so only the reap's SIGKILL ends it (the master's close SIGHUPs
/// the session leader alone). Control: a shell the predecessor still owns (an adoption
/// before Commit) is left alive, as its close leaves it.
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

    pool.hang_up_at_exit();
    let settled = aterm_pty::wait_for_reaps(Duration::from_secs(60));
    // A hang detector: the reap's SIGKILL is the event.
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

/// THE COLD UPDATE LANE'S `exec` LEAVES NO HUP-IGNORING SHELL OF A CLOSED TAB
/// . The cold lane runs only at zero live tabs, but a tab
/// closed just before keeps its shell's reap in grace for ~250 ms, and `exec`
/// ends the reap's thread with every other: a shell that ignores SIGHUP, not
/// yet SIGKILLed, would outlive the switch. The steps before the `exec`
/// (`App::prepare_cold_exec`) settle those reaps, so by the time the `exec`
/// runs no reap is in grace: each holdout has its SIGKILL.
///
/// Deterministic: the holdout says when its trap is set, so the hang-up cannot
/// land first, and the reap is held at its SIGKILL tick
/// (`aterm_pty::hold_reap_kills_for_test`) until the precondition is read, so a
/// descheduled test thread cannot outlast the grace. RED with the wait removed
/// from `prepare_cold_exec`: the exec runs while the released reap is still in
/// its grace. Runs alone in its own process: the reap count and the hold are
/// the process's.
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
    let hold = aterm_pty::hold_reap_kills_for_test();
    // Closing the tab: its hang-up, and the reap in grace.
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
    // A hang detector: the reap's SIGKILL is the event.
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
