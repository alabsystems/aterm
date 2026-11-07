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

/// Hangs up and reaps a parked `script`, recorded the adopted way so the escalation is authorized
/// by the start time alone, and requires that it died by the `SIGKILL` escalation. Answers its
/// poll waits and the tick of its `SIGKILL`.
fn hang_up_and_force_kill(script: &str) -> (u32, Option<u32>) {
    hang_up_and_force_kill_once(script, |_| {})
}

/// [`hang_up_and_force_kill`] in teardown's order, with `settled` run between the hang-up and the
/// reap.
fn hang_up_and_force_kill_once(script: &str, settled: impl FnOnce(i32)) -> (u32, Option<u32>) {
    let sh = parked_session(script);
    let identity = record_adopted_shell(sh.pid, sh.master);
    assert!(identity.verify(), "the live leader verifies");
    let ignored_hangup = ignores_hangup(&identity);
    let hung = hangup_shell(&identity, sh.master);
    settled(sh.pid);
    let polls_before = reap_polls();
    let _ = take_reap_kill_tick();
    reap_shell_scheduled(
        identity,
        Some(REAP_KILL_AT),
        REAP_DEADLINE,
        InGrace::enter(),
        ignored_hangup,
    );
    let polls = reap_polls() - polls_before;
    let kill_tick = take_reap_kill_tick();
    // Reading the terminal meanwhile: a shell's last words, unread, hold its exit in the tty drain.
    let mut status: libc::c_int = 0;
    let mut collected = 0;
    let deadline = Instant::now() + HANG_DETECTOR;
    while collected == 0 && Instant::now() < deadline {
        let _ = read(sh.master, &mut [0u8; 256]);
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
        "the shell must have died by the SIGKILL escalation \
         (waitpid={collected}, status=0x{status:x})"
    );
    (polls, kill_tick)
}

/// One shell the kernel says ignores `SIGHUP` and one whose trap never returns, the escalation
/// counted in the reaper's own ticks. The first row is what proves the mask reads true: a wrong
/// offset or an unread record reads as "not ignoring" and moves it to the handler tick.
#[test]
fn a_verified_shell_is_hung_up_and_force_killed_only_past_its_grace() {
    let (polls, kill_tick) = hang_up_and_force_kill("trap '' HUP; printf 'UP\\n'; exec sleep 30");
    assert_eq!(
        kill_tick,
        Some(REAP_KILL_AT),
        "a HUP-ignoring holdout is SIGKILLed at the holdout tick"
    );
    assert!(
        polls > REAP_KILL_AT && polls < REAP_DEADLINE,
        "the escalation, not the give-up, ended the wait \
         ({polls} poll waits, kill at {REAP_KILL_AT}, give-up at {REAP_DEADLINE})"
    );
    let (polls, kill_tick) =
        hang_up_and_force_kill("trap 'read go; exit 0' HUP; printf 'UP\\n'; sleep 30; exit 0");
    assert_eq!(
        kill_tick,
        Some(REAP_HANDLER_KILL_AT),
        "a shell still running its SIGHUP handler is SIGKILLed only once the handler's grace \
         is spent, never at the holdout tick"
    );
    assert!(
        polls > REAP_HANDLER_KILL_AT && polls < REAP_DEADLINE,
        "the escalation, not the give-up, ended the wait \
         ({polls} poll waits, kill at {REAP_HANDLER_KILL_AT}, give-up at {REAP_DEADLINE})"
    );
}

/// A handler that ignores `SIGHUP` for its own cleanup, as `tools/install.sh`'s does, reads as
/// ignoring by the holdout tick. What it did before the hang-up is what names a holdout.
#[test]
fn a_handler_that_ignores_sighup_for_its_own_cleanup_is_no_holdout() {
    let (_, kill_tick) = hang_up_and_force_kill_once(
        "trap 'trap \"\" HUP; read go; exit 0' HUP; printf 'UP\\n'; sleep 30 & wait",
        |pid| {
            let deadline = Instant::now() + HANG_DETECTOR;
            while !aterm_uds::process::ignored_signals(pid)
                .is_some_and(|mask| mask & (1 << (libc::SIGHUP - 1)) != 0)
            {
                assert!(Instant::now() < deadline, "the handler never ran");
                std::thread::sleep(Duration::from_millis(5));
            }
        },
    );
    assert_eq!(kill_tick, Some(REAP_HANDLER_KILL_AT));
}

/// A real shell at its prompt with a hang-up handler: what runs it, and the rc file that sets the
/// handler to do `work` before writing `marker`.
#[cfg(target_os = "macos")]
fn handler_row(
    scratch: &std::path::Path,
    name: &str,
    work: &str,
    marker: &std::path::Path,
) -> (String, std::path::PathBuf, String) {
    let marker = marker.display();
    let dir = scratch.join(name);
    std::fs::create_dir_all(&dir).expect("the row's folder");
    match name {
        "zsh-trap" => (
            format!("ZDOTDIR='{}' exec /bin/zsh -d -i", dir.display()),
            dir.join(".zshrc"),
            format!("trap '{work}; print done > {marker}; exit 0' HUP\nprint UP\n"),
        ),
        "zsh-zshexit" => (
            format!("ZDOTDIR='{}' exec /bin/zsh -d -i", dir.display()),
            dir.join(".zshrc"),
            format!("zshexit() {{ {work}; print done > {marker} }}\nprint UP\n"),
        ),
        "bash-exit-trap" => (
            format!(
                "exec /bin/bash --noprofile --rcfile '{}/rc' -i",
                dir.display()
            ),
            dir.join("rc"),
            // Announce readiness from the first prompt, after Bash has installed its
            // interactive signal handlers. An `echo` in the rc file races that setup.
            format!("unset HISTFILE\ntrap '{work}; echo done > {marker}' EXIT\nPS1=UP\n"),
        ),
        other => panic!("no row {other}"),
    }
}

/// Alive at the holdout tick is not "ignored `SIGHUP`". The handler's work waits on a line this
/// test writes only once a schedule that ends just past that tick has returned, so a shell cut
/// there is counted, never raced.
#[cfg(target_os = "macos")]
#[test]
fn a_real_shell_finishes_its_hang_up_handler_past_the_holdout_tick() {
    use std::os::unix::ffi::OsStrExt as _;
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let mut cut = Vec::new();
    for name in ["zsh-trap", "zsh-zshexit", "bash-exit-trap"] {
        let go = scratch.path().join(format!("{name}.go"));
        let fifo = std::ffi::CString::new(go.as_os_str().as_bytes()).expect("a path has no NUL");
        // SAFETY: `fifo` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0, "mkfifo");
        // Read and write, so neither end's open waits for the other.
        // SAFETY: `fifo` is a valid NUL-terminated path.
        let gate = unsafe { libc::open(fifo.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        assert!(gate >= 0, "the go line's fifo opens");
        let marker = scratch.path().join(format!("{name}.marker"));
        let work = format!("read line < {}", go.display());
        let (script, rc, text) = handler_row(scratch.path(), name, &work, &marker);
        std::fs::write(rc, text).expect("the rc file");
        let sh = parked_session(&script);
        let identity = record_adopted_shell(sh.pid, sh.master);
        assert!(identity.verify(), "the live leader verifies");
        let hung = hangup_shell(&identity, sh.master);
        assert_eq!(hung, ShellHangup::Shell(sh.pid), "{name} is hung up");
        reap_shell_scheduled(
            identity,
            Some(REAP_KILL_AT),
            REAP_KILL_AT + 1,
            InGrace::enter(),
            false,
        );
        // SAFETY: writing a few bytes from a valid buffer to the fifo this test opened.
        let wrote = unsafe { libc::write(gate, b"go\n".as_ptr().cast(), 3) };
        assert_eq!(wrote, 3, "the go line reaches the fifo");
        // Until the marker or the shell's end: zsh at its prompt runs a trap's `exit` only on
        // its next input, which never comes.
        let mut status: libc::c_int = 0;
        let mut collected = 0;
        let deadline = Instant::now() + HANG_DETECTOR;
        while collected == 0 && !marker.exists() && Instant::now() < deadline {
            let _ = read(sh.master, &mut [0u8; 256]);
            // SAFETY: non-blocking wait on the test's own child.
            collected = unsafe { libc::waitpid(sh.pid, &mut status, libc::WNOHANG) };
            std::thread::sleep(Duration::from_millis(5));
        }
        if !marker.exists() {
            cut.push(format!("{name} (status=0x{status:x})"));
        }
        // SAFETY: closing the descriptors this test owns, then ending and reaping its own child
        // if it is still there.
        unsafe {
            libc::close(gate);
            libc::close(sh.master);
            if collected == 0 {
                libc::kill(sh.pid, libc::SIGKILL);
                libc::waitpid(sh.pid, std::ptr::null_mut(), 0);
            }
        }
    }
    assert!(
        cut.is_empty(),
        "the reaper cut these shells mid-handler at the holdout tick, their markers unwritten: \
         {cut:?}"
    );
}

/// Whether `fd`'s terminal has `CLOCAL` set.
#[cfg(target_os = "macos")]
fn clocal_set(fd: i32) -> bool {
    // SAFETY: every field of `termios` is an integer, so all-zero is valid.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `tcgetattr` reads the test's live master into the valid out-param `t`.
    unsafe { libc::tcgetattr(fd, &raw mut t) == 0 && t.c_cflag & libc::CLOCAL != 0 }
}

/// A fifo in `scratch` named `name`, opened read and write so neither end's open waits for the
/// other: its path and the descriptor.
#[cfg(target_os = "macos")]
fn gate_fifo(scratch: &std::path::Path, name: &str) -> (std::path::PathBuf, i32) {
    use std::os::unix::ffi::OsStrExt as _;
    let path = scratch.join(format!("{name}.fifo"));
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("a path has no NUL");
    // SAFETY: `c` is a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
    // SAFETY: `c` is a valid NUL-terminated path.
    let gate = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    assert!(gate >= 0, "the gate's fifo opens");
    (path, gate)
}

/// Tests in one binary hold reap kills at once, each dropping its own guard as its row ends: one
/// drop must leave the reap another test still holds at its kill tick, or that shell is killed
/// mid-handler and its marker never written. A hold that is a flag, not a count, fails here.
#[test]
fn a_hold_dropped_beside_another_keeps_the_other_holding() {
    let first = hold_reap_kills_for_test();
    let second = hold_reap_kills_for_test();
    drop(first);
    assert!(
        reap_kills_held(),
        "one hold's drop released the reap another hold still keeps"
    );
    drop(second);
}

/// Ends the test's own shell if it is still there and waits for teardown, which reaps it.
#[cfg(target_os = "macos")]
fn end_row(pid: i32, hold: ReapKillHold) {
    if child_exit_unreaped(pid) == Some(false) {
        // SAFETY: ending the test's own child, which teardown then reaps.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    drop(hold);
    assert!(wait_for_reaps(HANG_DETECTOR), "teardown completed");
}

/// Mid-handler, the master's close is end of file and no second hang-up: a `cat` the HUP trap runs
/// on the terminal reads end of file, and the trap goes on to write its marker. Teardown that kept
/// a duplicate of the master would leave `cat` waiting, and a close without `CLOCAL` would hang
/// zsh up again. Kills are held throughout, so no `SIGKILL` can be what ends `cat`. This is also
/// what ends zsh at its prompt holding a `trap ... HUP`'s `exit` until its next input (macOS
/// 27.0.1, ATERM-KI-1104-7).
#[cfg(target_os = "macos")]
#[test]
fn the_terminals_close_mid_handler_is_end_of_file_and_no_hang_up() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let hold = hold_reap_kills_for_test();
    let entered = scratch.path().join("zsh-trap.entered");
    let marker = scratch.path().join("zsh-trap.done");
    let work = format!("print entered > {}; cat > /dev/null", entered.display());
    let (script, rc, text) = handler_row(scratch.path(), "zsh-trap", &work, &marker);
    std::fs::write(rc, text).expect("the rc file");
    let sh = parked_session(&script);
    let identity = record_spawned_shell(sh.pid);
    assert!(identity.verify(), "the live leader verifies");
    let hung = hangup_and_reap_shell(identity, sh.master).expect("teardown starts");
    assert_eq!(hung, ShellHangup::Shell(sh.pid), "the shell is hung up");
    let deadline = Instant::now() + HANG_DETECTOR;
    while !entered.exists() && Instant::now() < deadline {
        let _ = read(sh.master, &mut [0u8; 256]);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(entered.exists(), "the trap ran");
    // SAFETY: the test owns the original master; teardown holds none on macOS, only the slave.
    unsafe {
        libc::close(sh.master);
    }
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let finished = marker.exists();
    end_row(sh.pid, hold);
    assert!(
        finished,
        "the trap's `cat` never read end of file, or zsh was hung up again: the master's close \
         was withheld or was a hang-up"
    );
}

/// A foreground job that writes back a termios without `CLOCAL` mid-handler, as a program exiting
/// on the hang-up writes back the one it saved at start, has the flag put back before the master's
/// close, and that close then cuts no `zshexit` or bash EXIT trap. A teardown that set the flag only
/// once, or never, leaves it clear.
#[cfg(target_os = "macos")]
#[test]
fn a_foreground_jobs_write_back_mid_handler_is_undone_before_the_close() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    for name in ["zsh-zshexit", "bash-exit-trap"] {
        let hold = hold_reap_kills_for_test();
        let (gate_path, gate) = gate_fifo(scratch.path(), name);
        let cleared = scratch.path().join(format!("{name}.cleared"));
        let marker = scratch.path().join(format!("{name}.done"));
        let work = format!(
            "stty -clocal; echo cleared > {}; read line < {}",
            cleared.display(),
            gate_path.display()
        );
        let (script, rc, text) = handler_row(scratch.path(), name, &work, &marker);
        std::fs::write(rc, text).expect("the rc file");
        let sh = parked_session(&script);
        let identity = record_spawned_shell(sh.pid);
        assert!(identity.verify(), "the live leader verifies");
        hangup_and_reap_shell(identity, sh.master).expect("teardown starts");
        let deadline = Instant::now() + HANG_DETECTOR;
        while !cleared.exists() && Instant::now() < deadline {
            let _ = read(sh.master, &mut [0u8; 256]);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(cleared.exists(), "{name}'s `stty -clocal` ran");
        while !clocal_set(sh.master) && Instant::now() < deadline {
            let _ = read(sh.master, &mut [0u8; 256]);
            std::thread::sleep(Duration::from_millis(5));
        }
        let restored = clocal_set(sh.master);
        // SAFETY: the test owns the original master and the gate's descriptor; the go line is a
        // few bytes from a valid buffer, written only once the master is closed.
        let wrote = unsafe {
            libc::close(sh.master);
            let wrote = libc::write(gate, b"go\n".as_ptr().cast(), 3);
            libc::close(gate);
            wrote
        };
        assert_eq!(wrote, 3, "the go line reaches the fifo");
        while restored && !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let finished = marker.exists();
        end_row(sh.pid, hold);
        assert!(
            restored,
            "teardown left `CLOCAL` clear after {name}'s foreground job cleared it"
        );
        assert!(finished, "the master's close cut {name}'s exit handler");
    }
}

/// The master's close, mid-handler, is end of file and no second hang-up, so a `zshexit` or bash
/// EXIT trap runs to its end; its output past the close fails, as nothing reads it.
#[cfg(target_os = "macos")]
#[test]
fn closing_the_original_master_mid_handler_cuts_no_exit_handler() {
    use std::os::unix::ffi::OsStrExt;
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    for name in ["zsh-zshexit", "bash-exit-trap"] {
        let hold = hold_reap_kills_for_test();
        let gate_path = scratch.path().join(format!("{name}.fifo"));
        let path = std::ffi::CString::new(gate_path.as_os_str().as_bytes()).unwrap();
        // SAFETY: this test creates and owns its FIFO and descriptor.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let gate = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        assert!(gate >= 0);
        let entered = scratch.path().join(format!("{name}.entered"));
        let marker = scratch.path().join(format!("{name}.done"));
        let work = format!(
            "echo entered > {}; read line < {}; printf '%s' '{}'",
            entered.display(),
            gate_path.display(),
            "x".repeat(128 * 1024)
        );
        let (script, rc, text) = handler_row(scratch.path(), name, &work, &marker);
        std::fs::write(rc, text).unwrap();
        let sh = parked_session(&script);
        let identity = record_spawned_shell(sh.pid);
        assert!(identity.verify());
        hangup_and_reap_shell(identity, sh.master).expect("teardown starts");
        // SAFETY: the foreground test owns the original master; teardown holds none on macOS.
        unsafe {
            libc::close(sh.master);
        }
        let deadline = Instant::now() + HANG_DETECTOR;
        while !entered.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            entered.exists(),
            "{name} entered its cleanup with the original master closed"
        );
        // SAFETY: write the gate only after the handler proved it entered cleanup.
        assert_eq!(unsafe { libc::write(gate, b"go\n".as_ptr().cast(), 3) }, 3);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        unsafe {
            libc::close(gate);
        }
        assert!(
            marker.exists(),
            "{name} ran its exit handler to its end past the master's close"
        );
        drop(hold);
        assert!(wait_for_reaps(HANG_DETECTOR), "teardown completed");
    }
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
    reap_shell_scheduled(identity, None, 2, InGrace::enter(), false);
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

/// Set only on the child [`a_quit_leaves_no_shell`] starts.
#[cfg(target_os = "macos")]
const QUITTING: &str = "ATERM_PTY_TEST_QUITTING";

#[cfg(target_os = "macos")]
#[test]
fn a_quit_after_wait_for_reaps_leaves_no_hup_ignoring_shell() {
    a_quit_leaves_no_shell("trap '' HUP; printf 'UP\\n'; exec sleep 600");
}

/// Its grace slot follows the escalation to the handler tick: given back at the holdout tick, the
/// quitting process would exit with this shell still running.
#[cfg(target_os = "macos")]
#[test]
fn a_quit_after_wait_for_reaps_leaves_no_shell_that_caught_its_hang_up_and_carried_on() {
    a_quit_leaves_no_shell("trap : HUP; printf 'UP\\n'; while :; do /bin/sleep 600; done");
}

/// A process hangs up a shell running `script`, waits for its reap and exits; the shell must not
/// outlive it. The calling test runs twice, as that process and as the one watching it.
#[cfg(target_os = "macos")]
fn a_quit_leaves_no_shell(script: &str) {
    if std::env::var_os(QUITTING).is_some() {
        let sh = parked_session(script);
        let identity = record_spawned_shell(sh.pid);
        let birth = identity.birth.expect("a live child reads a birth");
        println!("HOLDOUT {} {birth}", sh.pid);
        let ignored_hangup = ignores_hangup(&identity);
        let _ = hangup_shell(&identity, sh.master);
        reap_shell_in_background(identity, ignored_hangup);
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
        "the shell outlived the process that quit after hanging it up ({holdout:?})"
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
            hangup_and_reap_shell(shell, sh.master).expect("retain the closing terminal");
        }
        // SAFETY: close the original now, matching Session disposal while teardown owns its copy.
        unsafe { libc::close(sh.master) };
        if own_group {
            assert!(wait_for_reaps(HANG_DETECTOR));
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
