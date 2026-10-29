// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::{Duration, Instant};

const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// Spawn a real PTY session running `script` under `/bin/sh` and wait for `UP`.
fn parked_session(script: &str) -> SpawnedShell {
    // SAFETY: test process, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let exec: Vec<std::ffi::OsString> = vec!["/bin/sh".into(), "-c".into(), script.into()];
    let sh = spawn_shell_with_pid_cell_px(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None,
        None,
        None,
        Some(&exec),
        None,
        None,
        aterm_sandbox::Limits::inherit(),
        None,
    )
    .expect("the parked session must spawn");
    set_nonblocking(sh.master, true).expect("nonblocking master");
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !String::from_utf8_lossy(&seen).contains("UP") {
        let mut buf = [0u8; 64];
        let n = read(sh.master, &mut buf);
        if n > 0 {
            seen.extend_from_slice(&buf[..n as usize]);
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(
        String::from_utf8_lossy(&seen).contains("UP"),
        "the parked session never announced itself; the test would prove nothing"
    );
    sh
}

fn setsid_sleep() -> aterm_uds::spawnfd::Child {
    aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .setsid(true)
        .spawn()
        .expect("the bystander sleep must spawn")
}

fn end(mut child: aterm_uds::spawnfd::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn setsid_sleep_born_after(birth: Option<u64>) -> aterm_uds::spawnfd::Child {
    let deadline = Instant::now() + HANG_DETECTOR;
    loop {
        let child = setsid_sleep();
        let pid = i32::try_from(child.id()).expect("pid fits i32");
        let born = process_birth(pid);
        if (born.is_some() && born != birth) || Instant::now() >= deadline {
            return child;
        }
        end(child);
    }
}

#[test]
fn teardown_leaves_a_setsid_sleep_holding_the_recycled_pid_alive() {
    let sh = parked_session("printf 'UP\\n'; exec sleep 30");
    let recorded = record_adopted_shell(sh.pid, sh.master);
    assert!(
        recorded.birth.is_some() && !recorded.own_child,
        "PRECONDITION: a live adopted shell leading its PTY records a birth \
         ({recorded:?}); without one this test would pass for the wrong reason"
    );
    // SAFETY: signalling and reaping the test's own child.
    unsafe {
        libc::kill(sh.pid, libc::SIGKILL);
        libc::waitpid(sh.pid, std::ptr::null_mut(), 0);
    }

    let mut bystander = setsid_sleep_born_after(recorded.birth);
    let bystander_pid = i32::try_from(bystander.id()).expect("pid fits i32");
    // SAFETY: read-only identity probe of the test's own child.
    let pgid = unsafe { libc::getpgid(bystander_pid) };
    assert_eq!(
        pgid, bystander_pid,
        "NON-VACUITY: the bystander must pass the OLD guard (a group leader)"
    );
    let bystander_birth = process_birth(bystander_pid);
    assert!(
        bystander_birth.is_some() && bystander_birth != recorded.birth,
        "PRECONDITION: the bystander reads a birth after the shell's, as a reissued pid's \
         holder does ({bystander_birth:?}, the shell's {recorded:?})"
    );
    let stale = ShellIdentity::from_parts(bystander_pid, recorded.birth, false);
    let verified = stale.verify();

    let hung = hangup_shell(&stale, sh.master);
    let polls_before = reap_polls();
    reap_shell(stale);
    let reap_polls_taken = reap_polls() - polls_before;
    let still_running = matches!(bystander.try_wait(), Ok(None));
    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };
    end(bystander);

    assert!(
        still_running,
        "teardown SIGNALLED the unrelated process that now holds the shell's pid \
         (hangup_shell answered {hung:?})"
    );
    assert!(!verified, "a reissued pid must not verify");
    assert_eq!(
        hung,
        ShellHangup::Nothing,
        "the dead shell left no foreground group on its PTY, so nothing may be \
         hung up — least of all the recycled pid"
    );
    assert_eq!(
        reap_polls_taken, 0,
        "reap_shell must give up at once on a pid that is no longer the shell \
         (it waited {reap_polls_taken} poll periods)"
    );
}

#[test]
fn adoption_records_a_birth_only_for_the_ptys_session_leader() {
    let sh = parked_session("printf 'UP\\n'; exec sleep 30");
    let bystander = setsid_sleep();
    let bystander_pid = i32::try_from(bystander.id()).expect("pid fits i32");
    let leader = record_adopted_shell(sh.pid, sh.master);
    let stranger = record_adopted_shell(bystander_pid, sh.master);
    hangup(sh.pid);
    reap(sh.pid);
    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };
    end(bystander);
    assert!(
        leader.birth.is_some(),
        "the PTY's own leader records a birth"
    );
    assert_eq!(
        stranger,
        ShellIdentity::from_parts(bystander_pid, None, false),
        "a pid that does not lead the carried PTY records nothing"
    );
    assert!(!stranger.verify(), "and so it never verifies");
}

#[test]
fn a_verified_hup_ignoring_shell_is_still_hung_up_and_force_killed() {
    let sh = parked_session("trap '' HUP; printf 'UP\\n'; exec sleep 30");
    let identity = record_adopted_shell(sh.pid, sh.master);
    assert!(identity.verify(), "the live leader verifies");
    let hung = hangup_shell(&identity, sh.master);
    let polls_before = reap_polls();
    let _ = take_reap_kill_tick();
    reap_shell(identity);
    let polls = reap_polls() - polls_before;
    let kill_tick = take_reap_kill_tick();
    let mut status: libc::c_int = 0;
    let mut collected = 0;
    let deadline = Instant::now() + Duration::from_secs(60);
    while collected == 0 && Instant::now() < deadline {
        // SAFETY: non-blocking wait on the test's own child.
        collected = unsafe { libc::waitpid(sh.pid, &mut status, libc::WNOHANG) };
        if collected == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };
    assert_eq!(
        hung,
        ShellHangup::Shell(sh.pid),
        "the verified shell is hung up"
    );
    assert!(
        collected == sh.pid && libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL,
        "the HUP-ignoring shell must have died by the SIGKILL escalation \
         (waitpid={collected}, status=0x{status:x})"
    );
    assert_eq!(
        kill_tick,
        Some(REAP_KILL_AT),
        "the SIGKILL escalation goes out at the ~250 ms tick"
    );
    assert!(
        polls > REAP_KILL_AT && polls < REAP_DEADLINE,
        "the escalation, not the give-up, ended the wait \
         ({polls} poll waits, kill at {REAP_KILL_AT}, give-up at {REAP_DEADLINE})"
    );
}

#[test]
fn an_unreaped_own_child_zombie_still_verifies_and_is_reaped() {
    let sh = parked_session("printf 'UP\\n'; exec sleep 30");
    let identity = record_spawned_shell(sh.pid);
    assert!(identity.birth.is_some() && identity.own_child);
    // SAFETY: signalling the test's own child.
    unsafe { libc::kill(sh.pid, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(60);
    while process_birth(sh.pid).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(process_birth(sh.pid), None, "a zombie reads no birth");
    assert!(identity.verify(), "an unreaped own child still verifies");
    reap_shell(identity);
    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };
    assert!(!identity.verify(), "once reaped, the pid is no longer ours");
}

#[cfg(target_os = "macos")]
fn still_unreaped_after(pid: i32, patience: Duration) -> bool {
    let deadline = Instant::now() + patience;
    while is_unreaped_child(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    is_unreaped_child(pid)
}

#[cfg(target_os = "macos")]
#[test]
fn a_shell_stuck_exiting_on_unread_output_is_reaped_not_left_a_zombie() {
    let sh = parked_session("printf 'UP\\n'; read go; printf '%2000s' x; exec sleep 30");
    let identity = record_spawned_shell(sh.pid);
    assert!(identity.own_child, "a fresh spawn is our own child");
    let go = b"go\n";
    // SAFETY: writing a few bytes from a valid buffer to the master this test owns.
    let wrote = unsafe { libc::write(sh.master, go.as_ptr().cast(), go.len()) };
    assert_eq!(wrote, 3, "the go line reaches the shell");
    let deadline = Instant::now() + HANG_DETECTOR;
    while output_queue_len(sh.master).is_none_or(|n| n < 512) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        output_queue_len(sh.master).is_some_and(|n| n >= 512),
        "PRECONDITION: printf filled the tty output queue before hangup; got {:?}",
        output_queue_len(sh.master)
    );
    let hung = hangup_shell(&identity, sh.master);
    reap_shell(identity);
    let stuck_past_teardown = is_unreaped_child(sh.pid);
    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };
    let zombie = still_unreaped_after(sh.pid, HANG_DETECTOR);
    if zombie {
        // SAFETY: blocking reap of the test's own (now exited) child.
        unsafe { libc::waitpid(sh.pid, std::ptr::null_mut(), 0) };
    }
    assert_eq!(
        hung,
        ShellHangup::Shell(sh.pid),
        "the live shell is hung up"
    );
    assert!(
        stuck_past_teardown,
        "PRECONDITION: the hung-up shell sat in its exit's tty drain past \
         reap_shell's deadline; otherwise this proves nothing"
    );
    assert!(
        !zombie,
        "the shell was left an unreaped zombie after its master closed"
    );
}

#[test]
fn a_shell_alive_past_the_deadline_is_reaped_when_it_exits() {
    let sh = parked_session("printf 'UP\\n'; read go; exit 0");
    let identity = record_spawned_shell(sh.pid);
    reap_shell_scheduled(identity, None, 2, InGrace::enter());
    let alive_at_return = process_birth(sh.pid).is_some();
    let go = b"go\n";
    // SAFETY: writing a few bytes from a valid buffer to the master this test owns.
    let wrote = unsafe { libc::write(sh.master, go.as_ptr().cast(), go.len()) };
    let deadline = Instant::now() + HANG_DETECTOR;
    while is_unreaped_child(sh.pid) && Instant::now() < deadline {
        let mut buf = [0u8; 256];
        let _ = read(sh.master, &mut buf);
        std::thread::sleep(Duration::from_millis(10));
    }
    let zombie = is_unreaped_child(sh.pid);
    if zombie {
        // SAFETY: blocking reap of the test's own child, so no zombie outlives the failure.
        unsafe { libc::waitpid(sh.pid, std::ptr::null_mut(), 0) };
    }
    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };
    assert!(
        alive_at_return && wrote == 3,
        "PRECONDITION: the shell outlived the shortened schedule and got its \
         go line; otherwise this proves nothing about the deadline"
    );
    assert!(
        !zombie,
        "a shell past the deadline was left to become an unreaped zombie"
    );
}

/// Set only on the child [`a_quit_after_wait_for_reaps_leaves_no_hup_ignoring_shell`] starts.
#[cfg(target_os = "macos")]
const QUITTING: &str = "ATERM_PTY_TEST_QUITTING";

#[cfg(target_os = "macos")]
#[test]
fn a_quit_after_wait_for_reaps_leaves_no_hup_ignoring_shell() {
    if std::env::var_os(QUITTING).is_some() {
        let sh = parked_session("trap '' HUP; printf 'UP\\n'; exec sleep 600");
        let identity = record_spawned_shell(sh.pid);
        let birth = identity.birth.expect("a live child reads a birth");
        println!("HOLDOUT {} {birth}", sh.pid);
        let _ = hangup_shell(&identity, sh.master);
        reap_shell_in_background(identity);
        let settled = wait_for_reaps(HANG_DETECTOR);
        std::process::exit(if settled { 0 } else { 3 });
    }
    let test = std::thread::current()
        .name()
        .expect("libtest names each test's thread after it")
        .to_string();
    let log = std::env::temp_dir().join(format!(
        "aterm-pty-quitting-{}-{}.log",
        std::process::id(),
        test.replace("::", "-")
    ));
    let out = std::fs::File::create(&log).expect("the child's log");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
        .args(["--exact", &test, "--nocapture", "--test-threads=1"])
        .env(QUITTING, "1")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(out.try_clone().expect("the log, twice"))
        .stderr(out)
        .spawn()
        .expect("start the quitting process");
    let status = aterm_spec::wait::within(HANG_DETECTOR, Duration::from_millis(5), || {
        child.try_wait().expect("the child's exit")
    });
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    let holdout = text
        .lines()
        .find_map(|line| {
            let mut words = line[line.find("HOLDOUT ")? + "HOLDOUT ".len()..].split(' ');
            let pid = words.next()?.parse().ok()?;
            let birth = words.next()?.parse().ok()?;
            Some(ShellIdentity::from_parts(pid, Some(birth), false))
        })
        .unwrap_or_else(|| panic!("the quitting process named no shell ({status:?}):\n{text}"));
    let gone = aterm_spec::wait::within(HANG_DETECTOR, Duration::from_millis(5), || {
        (!holdout.verify()).then_some(())
    });
    if gone.is_none() && holdout.verify() {
        // SAFETY: SIGKILL to the shell this test started, verified just above.
        unsafe { libc::kill(holdout.pid, libc::SIGKILL) };
    }
    assert!(
        status.is_some_and(|s| s.success()),
        "the quitting process: {status:?}\n{text}"
    );
    assert!(
        gone.is_some(),
        "the HUP-ignoring shell outlived the process that quit after hanging it up \
         ({holdout:?})"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_close_ends_a_plain_foreground_job_and_leaves_one_that_ignores_sighup() {
    for ignores in [false, true] {
        let trap = if ignores { "trap \"\" HUP; " } else { "" };
        let sh = parked_session(&format!(
            "set -m; /bin/sh -c '{trap}printf \"UP\\n\"; exec sleep 600'; :"
        ));
        // SAFETY: a read-only ioctl on the master this test owns.
        let foreground = unsafe { libc::tcgetpgrp(sh.master) };
        let job = ShellIdentity::from_parts(foreground, process_birth(foreground), false);
        let shell = record_spawned_shell(sh.pid);
        let own_group = foreground > 1 && foreground != sh.pid && job.verify();
        if own_group {
            let _ = hangup_shell(&shell, sh.master);
            reap_shell(shell);
        }
        let survived = own_group && !is_unreaped_child(sh.pid) && job.verify();
        let died = own_group
            && !ignores
            && aterm_spec::wait::within(HANG_DETECTOR, Duration::from_millis(5), || {
                (!job.verify()).then_some(())
            })
            .is_some();
        if job.verify() {
            // SAFETY: SIGKILL to the job this test started, verified just above.
            unsafe { libc::kill(job.pid, libc::SIGKILL) };
        }
        if is_unreaped_child(sh.pid) {
            // SAFETY: SIGKILL and a blocking reap of the test's own child.
            unsafe {
                libc::kill(sh.pid, libc::SIGKILL);
                libc::waitpid(sh.pid, std::ptr::null_mut(), 0);
            }
        }
        // SAFETY: closing the master this test owns.
        unsafe { libc::close(sh.master) };
        assert!(
            own_group,
            "PRECONDITION: the job is the terminal's foreground group, not the \
             shell's (foreground={foreground}, shell={})",
            sh.pid
        );
        if ignores {
            assert!(survived, "a job that ignores SIGHUP outlives the close");
        } else {
            assert!(died, "a plain foreground job dies with the close");
        }
    }
}
