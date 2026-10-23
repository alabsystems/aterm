// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows ConPTY end-to-end smoke tests.
//!
//! STOCK EXCEPTION: Windows only, and Trust has no Windows std, so only stock Rust builds this (the
//! Windows lane's guest pins its version: `aterm-lab/src/windows_guest.rs`).

#![cfg(windows)]

use std::io;
use std::sync::mpsc;
use std::time::Duration;

/// Hard per-test wall-clock budget. GENEROUS on purpose: the slowest body
/// (interactive cmd.exe + `ping -n 2` ≈ seconds) finishes in well under a
/// minute on a loaded machine, so a trip here is a real wedge, never flake.
const DEADLINE: Duration = Duration::from_secs(120);

/// Run `body` on a named thread and fail HARD if it outlives [`DEADLINE`].
fn run_with_deadline(name: &str, body: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel::<()>();
    let worker = std::thread::Builder::new()
        .name(format!("deadline:{name}"))
        .spawn(move || {
            body();
            let _ = tx.send(());
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
            // Re-raise worker panic.
            match worker.join() {
                Ok(()) => unreachable!("worker completed without sending its completion signal"),
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "windows_smoke::{name} exceeded {DEADLINE:?}: a ReadFile never returned; \
             child exit or hangup did not reach ClosePseudoConsole, so no EOF came \
             (the parked worker is reaped at process exit)"
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
fn read_until(master: i32, marker: &[u8], max_reads: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    for _ in 0..max_reads {
        let n = aterm_pty::read(master, &mut buf);
        if n <= 0 {
            // EOF (0) or error (<0) — the reader-exit path
            return (out, true);
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
            None,
            None,
            None,
            Some(&[
                "cmd.exe".into(),
                "/c".into(),
                "echo ATERM-WIN-MARKER".into(),
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
        assert!(
            drain_to_eof(master, 200),
            "reader must observe EOF after child exit (waiter-thread ClosePseudoConsole)"
        );

        let t0 = std::time::Instant::now();
        aterm_pty::reap(pid);
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "reap must return within its bounded budget"
        );

        aterm_pty::close_master(master);
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

#[test]
fn conpty_write_path_echoes_interactively() {
    run_with_deadline("conpty_write_path_echoes_interactively", || {
        let (spawn_cap, sandbox_cap) = caps();
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&["cmd.exe".into()]),
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
        assert_eq!(
            aterm_pty::exit_code(pid),
            Some(0),
            "cmd.exe `exit` must record exit code 0"
        );
        aterm_pty::reap(pid);
        aterm_pty::close_master(master);
    });
}

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
            None,
            None,
            None,
            Some(&["C:\\nonexistent\\aterm-no-such.exe".into()]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect_err("a nonexistent -e program must fail the spawn, not hand back a session");
        assert_eq!(
            err.kind(),
            io::ErrorKind::NotFound,
            "CreateProcessW not-found must surface as NotFound: {err}"
        );
    });
}

#[test]
fn bare_cmd_shim_resolves_via_pathext_and_launches() {
    run_with_deadline("bare_cmd_shim_resolves_via_pathext_and_launches", || {
        let Some(sysroot) = std::env::var_os("SystemRoot") else {
            aterm_spec::host::could_not_run("SystemRoot is not set");
        };
        let shim = std::path::Path::new(&sysroot)
            .join("System32")
            .join("winrm.cmd");
        if !shim.is_file() {
            aterm_spec::host::could_not_run(format_args!("{} is not on this SKU", shim.display()));
        }
        let (spawn_cap, sandbox_cap) = caps();
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&["winrm".into()]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("bare .cmd shim must spawn via the PATHEXT resolution retry");
        let (master, pid) = (shell.master, shell.pid);
        assert!(
            drain_to_eof(master, 400),
            "reader must observe EOF after the .cmd child exits"
        );
        aterm_pty::reap(pid);
        aterm_pty::close_master(master);
    });
}

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
            None,
            None,
            None,
            Some(&[
                "cmd.exe".into(),
                "/c".into(),
                "ping -n 2 127.0.0.1 > nul & echo ATERM-RESIZE-DONE".into(),
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
            None,
            None,
            None,
            Some(&["cmd.exe".into()]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("ConPTY spawn of bare cmd.exe must succeed");
        let (master, pid) = (shell.master, shell.pid);

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
/// style).
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
            None,
            None,
            None,
            Some(&["cmd.exe".into()]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("ConPTY spawn of bare cmd.exe must succeed");
        let (master, pid) = (shell.master, shell.pid);

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
