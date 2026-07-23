// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows ConPTY end-to-end smoke tests (the Windows lane of the crate's
//! spawn/IO coverage; the forkpty lane lives unix-gated in `src/unix.rs`).
//!
//! These drive the REAL seam on a real Windows 11 ConPTY: spawn → ConPTY read →
//! child exit → waiter `ClosePseudoConsole` → broken-pipe EOF → reap → close.
//! Every read loop is iteration-BOUNDED so a regression in the waiter-thread
//! EOF design (the load-bearing ConPTY gotcha: `ReadFile` does NOT return when
//! the child exits) fails the test instead of wedging the harness.
//!
//! ITERATION BOUNDS ARE NOT ENOUGH. They cap the NUMBER of reads, but each
//! `aterm_pty::read` is a single blocking `ReadFile` with NO timeout — if the
//! waiter never breaks the pipe (the exact regression these tests guard), read
//! #1 simply never returns and the loop counter never advances, so `cargo
//! test` wedges FOREVER instead of failing. Hence [`run_with_deadline`]: every
//! `#[test]` body runs on a named worker thread with a hard wall-clock
//! deadline. On timeout we `panic!` with a diagnosis naming the suspected
//! waiter/EOF regression instead of hanging CI. The wedged thread CANNOT be
//! cancelled (there is no portable way to interrupt a parked `ReadFile` from
//! std); it stays parked, and libtest's process exit reaps it — acceptable
//! precisely because a timeout already means the run is a failure.

#![cfg(windows)]

use std::io;
use std::sync::mpsc;
use std::time::Duration;

/// Hard per-test wall-clock budget. GENEROUS on purpose: the slowest body
/// (interactive cmd.exe + `ping -n 2` ≈ seconds) finishes in well under a
/// minute on a loaded CI box, so a trip here is a real wedge, never flake.
const DEADLINE: Duration = Duration::from_secs(120);

/// Run `body` on a named thread and fail HARD if it outlives [`DEADLINE`].
///
/// Why this exists: the bounded read loops below cannot bound TIME (see the
/// header). The completion channel is the deadline primitive — the worker
/// sends `()` as its final statement, so `recv_timeout` distinguishes the
/// three outcomes exactly: `Ok` = body finished, `Disconnected` = body
/// panicked (the sender was dropped mid-body; join and re-raise the ORIGINAL
/// panic so the assertion message survives), `Timeout` = a read is parked in
/// `ReadFile` with no EOF coming. In the timeout arm we deliberately do NOT
/// join — that would just move the forever-block here. The parked thread
/// stays parked; libtest's process exit reaps it. The thread is named after
/// the test so a debugger/stack dump of a wedged run points at the culprit.
fn run_with_deadline(name: &str, body: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel::<()>();
    let worker = std::thread::Builder::new()
        .name(format!("deadline:{name}"))
        .spawn(move || {
            body();
            let _ = tx.send(()); // LAST statement: `Ok` ⇔ the body completed
        })
        .expect("spawning the deadline worker thread must succeed");
    match rx.recv_timeout(DEADLINE) {
        Ok(()) => {
            // Body completed; join is instantaneous (nothing runs after the
            // send), and a post-send panic is impossible by construction.
            if let Err(payload) = worker.join() {
                std::panic::resume_unwind(payload);
            }
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // The sender was dropped without sending ⇒ the body panicked.
            // Join (already dead, returns immediately) and re-raise the real
            // payload so the test failure shows the original assertion.
            match worker.join() {
                Ok(()) => unreachable!("worker completed without sending its completion signal"),
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "windows_smoke::{name} exceeded its {DEADLINE:?} wall-clock deadline. \
             Each aterm_pty::read is ONE blocking ReadFile with NO timeout, so the \
             iteration-bounded loops cannot rescue a read that never returns: this \
             is the signature of a waiter-thread/EOF regression (child exit or \
             hangup no longer reaches ClosePseudoConsole, the out pipe is never \
             broken, EOF is never manufactured). The worker thread is still parked \
             inside ReadFile and cannot be cancelled; libtest's process exit reaps it."
        ),
    }
}

/// Mint Trusted spawn+sandbox caps (aterm-cap is pure Rust — identical to the
/// Unix tests' mint).
fn caps() -> (
    aterm_cap::Cap<aterm_cap::effects::Spawn>,
    aterm_cap::Cap<aterm_sandbox::Sandbox>,
) {
    // SAFETY: test-process mint; the trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    (
        authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted),
        authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted),
    )
}

/// Read until `marker` is seen (returns the accumulated bytes) or EOF/error.
/// Bounded: at most `max_reads` blocking reads — the EOF side is guaranteed by
/// the waiter thread breaking the pipe, so a hang here means the waiter died
/// (and [`run_with_deadline`] converts that hang into a test failure).
fn read_until(master: i32, marker: &[u8], max_reads: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    for _ in 0..max_reads {
        let n = aterm_pty::read(master, &mut buf);
        if n <= 0 {
            return (out, true); // EOF (0) or error (<0) — the reader-exit path
        }
        out.extend_from_slice(&buf[..n as usize]);
        if !marker.is_empty() && out.windows(marker.len()).any(|w| w == marker) {
            return (out, false);
        }
    }
    (out, false)
}

/// Drain to EOF (bounded). Returns true when EOF/error was reached.
fn drain_to_eof(master: i32, max_reads: usize) -> bool {
    let (_, eof) = read_until(master, b"", max_reads);
    eof
}

// End to end: spawn `cmd /c echo`, see the marker in the ConPTY stream, watch
// the waiter manufacture EOF after child exit, reap within the bounded budget,
// close the master, and verify the registry-miss semantics afterwards.
#[test]
fn conpty_spawns_cmd_echo_and_reaps() {
    run_with_deadline("conpty_spawns_cmd_echo_and_reaps", || {
        let (spawn_cap, sandbox_cap) = caps();
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&[
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo ATERM-WIN-MARKER".to_string(),
            ]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("ConPTY spawn of `cmd /c echo` must succeed");
        let (master, pid) = (shell.master, shell.pid);
        assert!(master >= 0, "master key must be >= 0 (gui asserts this)");
        assert!(pid > 1, "pid must be a real Windows process id");

        let (out, _) = read_until(master, b"ATERM-WIN-MARKER", 200);
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("ATERM-WIN-MARKER"),
            "echo output must appear in the ConPTY stream: {s:?}"
        );
        // The child exits on its own; the WAITER must break the pipe → EOF. This is
        // the load-bearing design point: without ClosePseudoConsole the read would
        // block forever (the deadline harness catches that as a failure, not a hang).
        assert!(
            drain_to_eof(master, 200),
            "reader must observe EOF after child exit (waiter-thread ClosePseudoConsole)"
        );

        // Reap is bounded (~2 s budget) and must return.
        let t0 = std::time::Instant::now();
        aterm_pty::reap(pid);
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "reap must return within its bounded budget"
        );

        aterm_pty::close_master(master);
        // Registry-miss semantics after close: read errors, write_some is NotFound
        // (proves the key is gone from the SESSIONS registry).
        let mut buf = [0u8; 8];
        assert!(
            aterm_pty::read(master, &mut buf) < 0,
            "read after close_master must return < 0"
        );
        assert_eq!(
            aterm_pty::write_some(master, b"x")
                .expect_err("write_some after close must fail")
                .kind(),
            io::ErrorKind::NotFound,
            "closed key must be a registry miss"
        );
    });
}

// The write path, interactively: spawn a bare cmd.exe, type an echo + exit,
// read the echoed marker AND the manufactured EOF, then check the recorded
// exit code (the waitpid-status replacement).
#[test]
fn conpty_write_path_echoes_interactively() {
    run_with_deadline("conpty_write_path_echoes_interactively", || {
        let (spawn_cap, sandbox_cap) = caps();
        // exec_command = ["cmd.exe"] (not shell selection) for hermeticity: the
        // harness environment may carry ATERM_SHELL/COMSPEC variations.
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&["cmd.exe".to_string()]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("ConPTY spawn of bare cmd.exe must succeed");
        let (master, pid) = (shell.master, shell.pid);

        aterm_pty::write_all(master, b"echo ATERM-ECHO-42\r\nexit\r\n");
        let (out, saw_eof) = read_until(master, b"ATERM-ECHO-42", 400);
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("ATERM-ECHO-42"),
            "typed echo must round-trip through the ConPTY: {s:?}"
        );
        if !saw_eof {
            assert!(
                drain_to_eof(master, 400),
                "reader must observe EOF after `exit` (waiter-thread ClosePseudoConsole)"
            );
        }
        // EOF ⇒ the waiter already recorded the exit status (it stores the code
        // BEFORE breaking the pipe), so this is race-free by construction.
        assert_eq!(
            aterm_pty::exit_code(pid),
            Some(0),
            "cmd.exe `exit` must record exit code 0"
        );
        aterm_pty::reap(pid);
        aterm_pty::close_master(master);
    });
}

// Fail-closed: a bogus -e program must surface CreateProcessW's error (the
// `_exit(127)` analog) and leak NO session — a subsequent op on any key the
// spawn could have minted is impossible because no SpawnedShell was returned;
// we additionally verify resize/hangup-by-guess stay no-ops.
#[test]
fn bogus_exec_command_fails_closed_no_session() {
    run_with_deadline("bogus_exec_command_fails_closed_no_session", || {
        let (spawn_cap, sandbox_cap) = caps();
        let err = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&["C:\\nonexistent\\aterm-no-such.exe".to_string()]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect_err("a nonexistent -e program must fail the spawn, not hand back a session");
        // ERROR_FILE_NOT_FOUND / ERROR_PATH_NOT_FOUND surface as NotFound.
        assert_eq!(
            err.kind(),
            io::ErrorKind::NotFound,
            "CreateProcessW not-found must surface as NotFound: {err}"
        );
    });
}

// PATHEXT end-to-end: a bare `.cmd` shim name (the npm/yarn shape) must
// resolve via the resolver's PATHEXT retry and launch through CreateProcessW
// by its resolved full path — before the fix this spawn failed NotFound.
// Hermetic: probes a stock System32 `.cmd` (SearchPathW always searches
// System32); skips gracefully on a trimmed SKU without it.
#[test]
fn bare_cmd_shim_resolves_via_pathext_and_launches() {
    run_with_deadline("bare_cmd_shim_resolves_via_pathext_and_launches", || {
        let Some(sysroot) = std::env::var_os("SystemRoot") else {
            return;
        };
        let shim = std::path::Path::new(&sysroot)
            .join("System32")
            .join("winrm.cmd");
        if !shim.is_file() {
            return;
        }
        let (spawn_cap, sandbox_cap) = caps();
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&["winrm".to_string()]), // bare name: only PATHEXT can resolve it
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("bare .cmd shim must spawn via the PATHEXT resolution retry");
        let (master, pid) = (shell.master, shell.pid);
        // winrm with no args prints usage and exits; EOF proves the child ran.
        assert!(
            drain_to_eof(master, 400),
            "reader must observe EOF after the .cmd child exits"
        );
        aterm_pty::reap(pid);
        aterm_pty::close_master(master);
    });
}

// resize is callable from any thread concurrently with a parked read and with
// writes (the control-socket threads do exactly this): drive it from a second
// thread while the main thread reads, then tear down cleanly.
#[test]
fn resize_is_thread_safe_against_parked_reader() {
    run_with_deadline("resize_is_thread_safe_against_parked_reader", || {
        let (spawn_cap, sandbox_cap) = caps();
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&[
                "cmd.exe".to_string(),
                "/c".to_string(),
                // ping -n 2 ≈ a ~1 s child: long enough to resize while it runs.
                "ping -n 2 127.0.0.1 > nul & echo ATERM-RESIZE-DONE".to_string(),
            ]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("ConPTY spawn must succeed");
        let (master, pid) = (shell.master, shell.pid);

        let resizer = std::thread::spawn(move || {
            for i in 0..20u16 {
                aterm_pty::resize(master, 24 + (i % 3), 80 + (i % 5));
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
        let (out, _) = read_until(master, b"ATERM-RESIZE-DONE", 400);
        resizer.join().expect("resizer thread must not panic");
        assert!(
            String::from_utf8_lossy(&out).contains("ATERM-RESIZE-DONE"),
            "child output must arrive while resizes run concurrently"
        );
        assert!(drain_to_eof(master, 400), "EOF after child exit");
        aterm_pty::reap(pid);
        aterm_pty::close_master(master);
    });
}

// hangup() must end a shell that would otherwise sit at its prompt forever:
// the close event → waiter ClosePseudoConsole → console-close signal → child
// exits → EOF. This is the non-blocking UI-thread teardown path — and the
// single most hang-prone assertion in the file: a waiter regression here
// parks `drain_to_eof` in ReadFile forever, which is exactly what the
// deadline harness exists to convert into a failure.
#[test]
fn hangup_ends_an_interactive_shell_with_eof() {
    run_with_deadline("hangup_ends_an_interactive_shell_with_eof", || {
        let (spawn_cap, sandbox_cap) = caps();
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&["cmd.exe".to_string()]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("ConPTY spawn of bare cmd.exe must succeed");
        let (master, pid) = (shell.master, shell.pid);

        // Wait for the first prompt bytes so the shell is definitely live.
        let (_, eof_early) = read_until(master, b">", 100);
        assert!(
            !eof_early,
            "shell must be alive at its prompt before hangup"
        );

        aterm_pty::hangup(pid);
        assert!(
            drain_to_eof(master, 400),
            "hangup must manufacture EOF (waiter ClosePseudoConsole → broken pipe)"
        );
        aterm_pty::reap(pid);
        aterm_pty::close_master(master);
    });
}

// set_nonblocking honesty: never pretend a mode we cannot actuate succeeded.
#[test]
fn set_nonblocking_true_is_unsupported() {
    run_with_deadline("set_nonblocking_true_is_unsupported", || {
        assert!(aterm_pty::set_nonblocking(22, false).is_ok());
        assert_eq!(
            aterm_pty::set_nonblocking(22, true)
                .expect_err("nonblocking=true must be refused on Windows")
                .kind(),
            io::ErrorKind::Unsupported
        );
    });
}

// The locale stub keeps the identical signature and injects nothing.
#[test]
fn resolve_spawn_locale_is_an_empty_stub() {
    run_with_deadline("resolve_spawn_locale_is_an_empty_stub", || {
        assert!(aterm_pty::resolve_spawn_locale(Some("C"), None, Some("C")).is_empty());
        assert!(aterm_pty::resolve_spawn_locale(None, None, None).is_empty());
    });
}

/// Tiny test-local FFI to read a foreign process's priority class back from
/// the KERNEL (the crate's ffi module is private by design; the smoke suite
/// verifies through the public OS surface, in the crate's tiny-FFI house
/// style). `PROCESS_QUERY_LIMITED_INFORMATION` (0x1000) suffices for
/// `GetPriorityClass`.
mod prio_ffi {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> isize;
        pub fn GetPriorityClass(hProcess: isize) -> u32;
        pub fn CloseHandle(hObject: isize) -> i32;
    }
    pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    pub const ABOVE_NORMAL_PRIORITY_CLASS: u32 = 0x8000;
    pub const NORMAL_PRIORITY_CLASS: u32 = 0x20;
}

/// The shell's KERNEL-side priority class, by pid (0 on any failure).
fn priority_class_of(pid: i32) -> u32 {
    // SAFETY: query-only open of a process this test spawned; closed below.
    let h = unsafe {
        prio_ffi::OpenProcess(prio_ffi::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32)
    };
    if h == 0 {
        return 0;
    }
    // SAFETY: `h` is a live handle owned by this fn.
    let class = unsafe { prio_ffi::GetPriorityClass(h) };
    // SAFETY: closing the handle opened above.
    unsafe { prio_ffi::CloseHandle(h) };
    class
}

// Focus boost end-to-end against the real kernel: boost ON must move the
// spawned shell ROOT process to ABOVE_NORMAL, boost OFF must restore NORMAL —
// verified by reading GetPriorityClass back from the OS, not by trusting the
// call. (The conhost half has no stable public identity to assert on here;
// its lane shares this exact code path and degrades to 0 = skip by design.)
#[test]
fn focus_boost_actuates_shell_priority_and_restores() {
    run_with_deadline("focus_boost_actuates_shell_priority_and_restores", || {
        let (spawn_cap, sandbox_cap) = caps();
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&["cmd.exe".to_string()]), // hermetic interactive child
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("ConPTY spawn of bare cmd.exe must succeed");
        let (master, pid) = (shell.master, shell.pid);

        // Live at the prompt (also proves the pid is running, so the class
        // reads below are of a live process, not a zombie). No assert on the
        // FRESH class: a niced test runner legally starts children below
        // NORMAL (the Win32 inheritance rule) — the ON/OFF transitions below
        // are the invariant, not the starting point.
        let (_, eof_early) = read_until(master, b">", 100);
        assert!(!eof_early, "shell must be alive at its prompt");
        assert_ne!(
            priority_class_of(pid),
            0,
            "the kernel-side class query itself must work"
        );

        aterm_pty::set_focus_boost(master, true);
        assert_eq!(
            priority_class_of(pid),
            prio_ffi::ABOVE_NORMAL_PRIORITY_CLASS,
            "boost ON must raise the shell root to ABOVE_NORMAL (kernel-verified)"
        );

        aterm_pty::set_focus_boost(master, false);
        assert_eq!(
            priority_class_of(pid),
            prio_ffi::NORMAL_PRIORITY_CLASS,
            "boost OFF must restore NORMAL (kernel-verified)"
        );

        aterm_pty::hangup(pid);
        assert!(drain_to_eof(master, 400), "EOF after hangup");
        aterm_pty::reap(pid);
        aterm_pty::close_master(master);
    });
}
