// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

/// A hang detector, never a latency budget: a reap ends the wait the moment
/// it lands.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// A real PTY session running `script` under `/bin/sh`, returned once it has
/// printed `UP` — so its shell is demonstrably alive and leads the PTY.
fn parked_session(script: &str) -> SpawnedShell {
    // SAFETY: test process, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let exec: Vec<String> = vec!["/bin/sh".into(), "-c".into(), script.into()];
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
    // A hang detector: the session parks only after printing `UP`, and the
    // loop leaves the moment it arrives.
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

/// The bystander: a `sleep` in its OWN session (`setsid`), so it is a
/// session leader and a group leader — the shape the old
/// `getpgid(pid) == pid` guard could not tell from a shell.
fn setsid_sleep() -> std::process::Child {
    let mut command = std::process::Command::new("/bin/sleep");
    command.arg("30");
    // SAFETY: `setsid` is async-signal-safe and touches no parent state;
    // it is the only thing run between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().expect("the bystander sleep must spawn")
}

fn end(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// THE INCIDENT SHAPE. An adopted tab's shell is recorded at adoption, then
/// exits (and is reaped by someone else — here the test, standing in for
/// launchd); the pid "comes back" as a `setsid`'d `sleep`; the tab closes.
/// Teardown must send the bystander nothing — neither the `SIGHUP` nor the
/// `SIGKILL` 250 ms later — and must not linger over it either.
#[test]
fn teardown_leaves_a_setsid_sleep_holding_the_recycled_pid_alive() {
    let sh = parked_session("printf 'UP\\n'; exec sleep 30");
    let recorded = record_adopted_shell(sh.pid, sh.master);
    assert!(
        recorded.birth.is_some() && !recorded.own_child,
        "PRECONDITION: a live adopted shell leading its PTY records a birth \
         ({recorded:?}); without one this test would pass for the wrong reason"
    );
    // The shell exits and its pid is freed (launchd's reap, played by us).
    // SAFETY: signalling and reaping the test's own child.
    unsafe {
        libc::kill(sh.pid, libc::SIGKILL);
        libc::waitpid(sh.pid, std::ptr::null_mut(), 0);
    }

    let mut bystander = setsid_sleep();
    let bystander_pid = i32::try_from(bystander.id()).expect("pid fits i32");
    // SAFETY: read-only identity probe of the test's own child.
    let pgid = unsafe { libc::getpgid(bystander_pid) };
    assert_eq!(
        pgid, bystander_pid,
        "NON-VACUITY: the bystander must pass the OLD guard (a group leader)"
    );
    // The kernel reissued the pid: the session still holds the recorded
    // birth, but its pid now names the bystander.
    let stale = ShellIdentity::from_parts(bystander_pid, recorded.birth, false);
    let verified = stale.verify();

    let hung = hangup_shell(&stale, sh.master);
    let polls_before = reap_polls();
    reap_shell(stale);
    let reap_polls_taken = reap_polls() - polls_before;
    // Past the old SIGKILL tick (250 ms), with margin.
    std::thread::sleep(Duration::from_millis(400));
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
    // COUNTED, not timed: "at once" is the loop's first verify returning
    // before a single poll wait. A stopwatch of 100 ms read a descheduled
    // test thread as a loop; the loop that does not give up waits every one
    // of its ticks, and each one is counted here.
    assert_eq!(
        reap_polls_taken, 0,
        "reap_shell must give up at once on a pid that is no longer the shell \
         (it waited {reap_polls_taken} poll periods)"
    );
}

/// The adoption proof itself: `record_adopted_shell` records a birth only
/// for the pid that leads the carried PTY. A pid that does not — the
/// bystander, standing in for a shell that died before adoption and whose
/// number was reissued — records none and so never verifies.
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

/// POSITIVE CONTROL — the historical teardown survives for a VERIFIED
/// shell (`docs/DESIGN-pty-keeper-2026-09-26.md` F3): its group is hung up,
/// and one that ignores `SIGHUP` is still force-killed at the ~250 ms tick.
/// Recorded the adopted way, so the escalation is authorized by the start
/// time alone.
#[test]
fn a_verified_hup_ignoring_shell_is_still_hung_up_and_force_killed() {
    let sh = parked_session("trap '' HUP; printf 'UP\\n'; exec sleep 30");
    let identity = record_adopted_shell(sh.pid, sh.master);
    assert!(identity.verify(), "the live leader verifies");
    let hung = hangup_shell(&identity, sh.master);
    let polls_before = reap_polls();
    let _ = take_reap_kill_tick();
    let started = Instant::now();
    reap_shell(identity);
    let took = started.elapsed();
    let polls = reap_polls() - polls_before;
    let kill_tick = take_reap_kill_tick();
    // Recorded the ADOPTED way, `reap_shell` stops the moment the shell no
    // longer verifies (launchd reaps an adopted shell's corpse), so the test
    // — its real parent — collects it here and reads HOW it died. libproc
    // stops answering for a process as its SIGKILL lands (~10 µs on Darwin
    // 25.6), but the corpse is waitable only a few hundred µs later when
    // idle and tens of ms later under load: wait for it (a hang detector,
    // not a budget).
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
    // It outlived SIGHUP: the loop cannot reach the kill tick before
    // `REAP_KILL_AT` poll waits, each at least `REAP_POLL`, so this lower
    // bound holds at any load. And it died at the ~250 ms ESCALATION, which
    // is COUNTED in the loop's own ticks — the SIGKILL went out at the kill
    // tick, and the loop saw the death before its give-up tick — rather
    // than timed: a descheduled test thread stretches the ticks, never
    // their number, and it outlasted the old 1.5 s stopwatch.
    assert!(
        took >= REAP_POLL * REAP_KILL_AT,
        "it outlived SIGHUP (took {took:?})"
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

/// A spawned shell that has exited but is not yet reaped is a ZOMBIE, whose
/// start time libproc refuses (measured 2026-09-26). It is still ours — the
/// kernel cannot reissue a pid we have not waited for — so it still
/// verifies, and `reap_shell` collects it rather than leaving a zombie.
#[test]
fn an_unreaped_own_child_zombie_still_verifies_and_is_reaped() {
    let sh = parked_session("printf 'UP\\n'; exec sleep 30");
    let identity = record_spawned_shell(sh.pid);
    assert!(identity.birth.is_some() && identity.own_child);
    // SAFETY: signalling the test's own child.
    unsafe { libc::kill(sh.pid, libc::SIGKILL) };
    // A hang detector for the SIGKILL to land (the zombie reads no birth).
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

/// Wait (a hang detector, not a budget) until `pid` is no longer this
/// process's unreaped child, and answer whether it still is — so a test
/// can tell "reaped" from "left a zombie" without reaping it itself.
fn still_unreaped_after(pid: i32, patience: Duration) -> bool {
    let deadline = Instant::now() + patience;
    while is_unreaped_child(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    is_unreaped_child(pid)
}

/// THE WINDOW'S ZOMBIE (2026-09-29: pid 61283, a child of the owner's
/// window born the second a fresh tab spawned, `<defunct>` hours later).
///
/// A session leader that exits with output still queued on its terminal
/// does not become a zombie: its exit sleeps in the tty drain until someone
/// reads the master or closes it, and teardown stops the reader before the
/// shell has gone. No signal ends that sleep (`killpg` answers `EPERM`, a
/// `kill(pid, SIGKILL)` changes nothing), so `reap_shell` reached its
/// deadline and returned; when the master's last holder let go, the shell
/// became a zombie nobody would ever wait for.
///
/// NEGATIVE CONTROL: without the hand-off to the straggler reaper at the
/// deadline, this fails with the shell still unreaped after the master
/// closed (measured before the fix: "left an unreaped zombie").
///
/// macOS only: the exit that sleeps in the tty drain is XNU's (measured on
/// Darwin 25.6). On Linux a slave write goes straight into the master's
/// buffer and nothing waits on close, so the hung-up shell dies at once and
/// the PRECONDITION could not hold; the deadline hand-off itself is covered
/// on every Unix by `a_shell_alive_past_the_deadline_is_reaped_when_it_exits`.
#[cfg(target_os = "macos")]
#[test]
fn a_shell_stuck_exiting_on_unread_output_is_reaped_not_left_a_zombie() {
    // `UP`, then — only once the test says `go`, after its own read of the
    // master has ended — output nobody reads, then a parked foreground.
    let sh = parked_session("printf 'UP\\n'; read go; printf '%2000s' x; exec sleep 30");
    let identity = record_spawned_shell(sh.pid);
    assert!(identity.own_child, "a fresh spawn is our own child");
    let go = b"go\n";
    // SAFETY: writing a few bytes from a valid buffer to the master this
    // test owns.
    let wrote = unsafe { libc::write(sh.master, go.as_ptr().cast(), go.len()) };
    assert_eq!(wrote, 3, "the go line reaches the shell");
    // The unread bytes (the echoed `go` and the 2000 spaces) land in the
    // terminal's output queue; `poll` sees them without consuming them.
    let queued = || {
        let mut pfd = libc::pollfd {
            fd: sh.master,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid `pollfd`, zero timeout.
        unsafe { libc::poll(&mut pfd, 1, 0) == 1 && pfd.revents & libc::POLLIN != 0 }
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while !queued() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // A beat for the rest of the printf to follow the first bytes.
    std::thread::sleep(Duration::from_millis(300));
    // Teardown, with the master still held and unread — as it is while a
    // closed tab's last `SinkWriter` clone has not dropped yet.
    let hung = hangup_shell(&identity, sh.master);
    reap_shell(identity);
    let stuck_past_teardown = is_unreaped_child(sh.pid);
    // The master's last holder lets go, later than `reap_shell`'s deadline.
    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };
    let zombie = still_unreaped_after(sh.pid, HANG_DETECTOR);
    if zombie {
        // Do not leave the zombie behind in the test process.
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

/// NEVER GIVEN UP ON: a spawned shell still alive at `reap_shell`'s
/// deadline — here one no escalation is sent to, standing in for a child
/// no signal can finish yet — is handed to the straggler reaper, which
/// waits for it when it does exit. Nothing in this test waits for it.
///
/// NEGATIVE CONTROL: without the hand-off at the deadline, the shell exits
/// on `go` into a zombie nobody collects, and this fails.
#[test]
fn a_shell_alive_past_the_deadline_is_reaped_when_it_exits() {
    let sh = parked_session("printf 'UP\\n'; read go; exit 0");
    let identity = record_spawned_shell(sh.pid);
    reap_shell_scheduled(identity, None, 2);
    let alive_at_return = process_birth(sh.pid).is_some();
    let go = b"go\n";
    // SAFETY: writing a few bytes from a valid buffer to the master this
    // test owns.
    let wrote = unsafe { libc::write(sh.master, go.as_ptr().cast(), go.len()) };
    // Read (and discard) what the shell echoes, so its exit never waits on
    // a drain, until the pid is no longer our unreaped child — a hang
    // detector, not a budget.
    let deadline = Instant::now() + HANG_DETECTOR;
    while is_unreaped_child(sh.pid) && Instant::now() < deadline {
        let mut buf = [0u8; 256];
        let _ = read(sh.master, &mut buf);
        std::thread::sleep(Duration::from_millis(10));
    }
    let zombie = is_unreaped_child(sh.pid);
    if zombie {
        // SAFETY: blocking reap of the test's own child, so no zombie
        // outlives the failure.
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

/// `reap_in_background` waits for a helper nobody else waits for. The
/// control — the same helper as a `std` child simply dropped, which is
/// what the window's link opener did — is still an unreaped zombie
/// afterwards.
#[test]
fn a_child_handed_to_the_reaper_is_reaped_and_a_dropped_one_is_not() {
    let dropped = std::process::Command::new("/usr/bin/true")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn /usr/bin/true");
    let dropped_pid = i32::try_from(dropped.id()).expect("pid fits i32");
    drop(dropped);
    let handed = aterm_uds::spawnfd::command("/usr/bin/true")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn /usr/bin/true");
    let handed_pid = i32::try_from(handed.id()).expect("pid fits i32");
    reap_in_background(handed);
    let handed_left = still_unreaped_after(handed_pid, HANG_DETECTOR);
    // The control has exited too by now (it is `true`), and stays ours.
    let dropped_left = is_unreaped_child(dropped_pid);
    // SAFETY: blocking reaps of the test's own children, so nothing
    // outlives the test.
    unsafe {
        libc::waitpid(dropped_pid, std::ptr::null_mut(), 0);
        if handed_left {
            libc::waitpid(handed_pid, std::ptr::null_mut(), 0);
        }
    }
    assert!(
        dropped_left,
        "CONTROL: a dropped `Child` is a zombie until someone waits for it"
    );
    assert!(!handed_left, "the handed-over child was never reaped");
}
