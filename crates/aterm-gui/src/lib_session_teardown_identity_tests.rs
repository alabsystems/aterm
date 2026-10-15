// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::{Duration, Instant};

/// A `sleep` in its own session: a session and group leader, so it passes
/// the old `getpgid(pid) == pid` guard exactly as a reissued shell pid
/// landing on a job-control leader or a daemon would.
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
    // Past the old SIGKILL tick (250 ms), with margin.
    std::thread::sleep(Duration::from_millis(500));
    let survived = running(&mut bystander);
    let _ = bystander.kill();
    let _ = bystander.wait();
    assert!(
        survived,
        "closing the tab SIGNALLED an unrelated process that holds its shell's old pid"
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

/// …AND SO ARE THE FILES THE PREDECESSOR HANDED THAT SHELL (round eight,
/// findings 20 and 44). The parent re-keyed the shell, or pointed it at its
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
/// drop removes them as before.
///
/// RED before the fix: both files were gone after the uncommitted drop.
#[test]
fn dropping_an_adopted_session_before_commit_leaves_the_predecessors_rekey_and_body_files() {
    let _env = crate::seamless::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        // The predecessor's files, written at ITS Commit.
        std::fs::write(&rekey, "parent-key\n").expect("the parent's re-key");
        std::fs::write(&body, "parent-body\n").expect("the parent's body pointer");
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
                !rekey.exists() && !body.exists(),
                "a committed session's untaken files go with it"
            );
            continue;
        }
        assert_eq!(
            (
                std::fs::read_to_string(&rekey).ok(),
                std::fs::read_to_string(&body).ok()
            ),
            (
                Some("parent-key\n".to_string()),
                Some("parent-body\n".to_string())
            ),
            "an uncommitted drop deleted the files the predecessor handed its shell"
        );
        // Its own held write went with the session: a release now leaves
        // the predecessor's pointer as it is.
        commit.release();
        assert_eq!(
            std::fs::read_to_string(&body).ok().as_deref(),
            Some("parent-body\n"),
            "a write held for a dropped session must not land"
        );
        let _ = std::fs::remove_file(&rekey);
        let _ = std::fs::remove_file(&body);
    }
}

/// THE LATCH `spawn_session` HANDS A SESSION decides that drop (round seven,
/// item 20): built through `spawn::adopted_handoff_commit` — the one
/// expression `spawn_session` sets `handoff_commit` with (pinned by
/// `spawn::adoption_wiring_tests::spawn_session_hands_an_adopted_session_the_handoff_s_commit_latch`)
/// — over a factory whose `rekey_deferred` is an overlap handoff's
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

/// A QUIT ENDS EVERY TAB'S SHELL AS CLOSING THE TAB DOES. The final exit forgets
/// the App, so no session is dropped; `hang_up_at_exit` runs each one's hang-up
/// and reap instead. This shell ignores SIGHUP, so only the reap's SIGKILL ends
/// it: before, the quit left it to the master's close, which SIGHUPs the
/// session leader alone, and it outlived aterm. CONTROL: a shell the
/// predecessor still owns (an adoption before Commit) is left alive, as its
/// close leaves it.
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
