// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P0 regression test: the daily-driver CLI must run a real shell through the
//! PROTECTED spawn seam (`aterm_pty::spawn_shell`, NOT raw `forkpty`/`execvp`) and
//! stay fully functional — given a command it produces the command's OUTPUT and
//! exits with the shell's status. Complements the static guard `A6` (no
//! `libc::forkpty` in `aterm-cli/src`) with a behavioral check that the protected
//! spawn actually works end-to-end.
//!
//! It also carries the SESSION-MODEL arming regression, which shares this file's
//! bounded-wait harness and drives the same binary down the same session path:
//! the in-process VT model is demand-driven and must stay OFF unless
//! `$ATERM_SESSION_MODEL` asks for it. The unit tests in `aterm-cli` pin the
//! arming RULE; only running the real binary can pin that `session_main`
//! consults it.

// The tests drive a POSIX `/bin/sh` through the binary; a `#[cfg(windows)]`
// twin (echo via cmd.exe) is the follow-up once the ConPTY seam lands.
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::process::{Child, Command, Output, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

/// How long the CLI gets to run the scripted shell session and exit. GENEROUS —
/// a healthy run finishes in well under a second even on a loaded CI box — because
/// the only thing this bound must catch is the exact regression these tests guard:
/// the CLI *not exiting*. `Child::wait_with_output` has no deadline and the
/// workspace gate (`cargo test`) has no per-test timeout, so without this bound a
/// doesn't-exit regression manifests as `cargo test` hanging forever instead of a
/// red test — the failure would hide inside the harness meant to detect it.
#[cfg(unix)]
const CLI_EXIT_DEADLINE: Duration = Duration::from_secs(60);

/// Bounded replacement for `Child::wait_with_output`, shared by every test in
/// this file. Dedicated threads drain the child's stdout/stderr pipes into
/// buffers (so the child can never stall on a full pipe while we wait), and the
/// main thread polls `try_wait` against `CLI_EXIT_DEADLINE`. On expiry the child
/// is killed, the drains are joined, and we panic with whatever output was
/// captured — turning a would-be infinite hang into a diagnosable failure.
#[cfg(unix)]
fn wait_with_output_bounded(mut child: Child) -> Output {
    // One drain thread per pipe. Tests that route a stream to `Stdio::null()`
    // simply have no handle here and the thread returns an empty buffer. A read
    // error (e.g. the deadline kill tearing the pipe down mid-read) just ends
    // the drain — partial output is still worth showing in the panic message.
    fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let deadline = Instant::now() + CLI_EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll the aterm CLI child") {
            Some(status) => {
                // The child is gone, so its pipe write-ends are closed and the
                // drains run to EOF promptly — these joins cannot hang. (The
                // grandchild shell lives on the PTY slave, not on these pipes.)
                return Output {
                    status,
                    stdout: stdout.join().expect("join the stdout drain"),
                    stderr: stderr.join().expect("join the stderr drain"),
                };
            }
            None if Instant::now() >= deadline => {
                // Kill closes the CLI's pipe write-ends, then reap so the process
                // table stays clean even though we are about to panic. But the
                // joins here are BOUNDED, unlike the success path: if a hung CLI
                // regression ALSO leaked the pipe write-end into some descendant
                // that outlives the kill, an unbounded `join` would block on that
                // descendant and the panic path itself would hang — the exact
                // failure shape this harness exists to eliminate. Bounded joins
                // guarantee we always reach the panic; worst case we report the
                // drain as still blocked instead of showing that stream.
                let _ = child.kill();
                let _ = child.wait();
                fn join_bounded(handle: std::thread::JoinHandle<Vec<u8>>) -> String {
                    let grace = Instant::now() + Duration::from_secs(5);
                    while !handle.is_finished() {
                        if Instant::now() >= grace {
                            return "<drain still blocked: pipe held open past kill>".into();
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    String::from_utf8_lossy(&handle.join().expect("join a finished drain"))
                        .into_owned()
                }
                let out = join_bounded(stdout);
                let err = join_bounded(stderr);
                panic!(
                    "CLI did not exit within {}s — the doesn't-exit regression this test \
                     guards against; captured stdout={out:?} stderr={err:?}",
                    CLI_EXIT_DEADLINE.as_secs()
                );
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

#[cfg(unix)]
#[test]
fn cli_runs_a_command_through_the_protected_spawn_and_exits_cleanly() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aterm"))
        .arg("--session") // piped stdio must still be the SESSION, not the window
        .env("SHELL", "/bin/sh") // a known POSIX shell — env-independent
        .env_remove("ATERM_CONTAINMENT_MODE") // default User mode: no sandbox, fast
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the aterm CLI binary");

    // Feed a command whose output PROVES the shell evaluated it (the arithmetic
    // `$((6*7))` becomes 42 only if a real shell ran it — the PTY echo of the input
    // line still shows the literal `$((6*7))`), then exit. Dropping stdin after the
    // write delivers EOF so the shell runs to its own exit.
    child
        .stdin
        .take()
        .expect("aterm stdin")
        .write_all(b"echo ATERM_P0_MARKER_$((6*7))\nexit\n")
        .expect("write to aterm stdin");

    let out = wait_with_output_bounded(child);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ATERM_P0_MARKER_42"),
        "the shell did not evaluate the command through the protected spawn; stdout={stdout:?}"
    );
    assert!(
        out.status.success(),
        "aterm must exit with the shell's success status; got {:?}",
        out.status
    );
}

/// THE DEFAULT: a session builds NO VT model. The daily driver used to construct
/// a full `Terminal` and feed it every PTY byte — an O(bytes) parse and
/// O(scrollback) memory — for a model nothing in the process could read, and a
/// change that quietly restores that default would be invisible in every other
/// test in this repo. The `$ATERM_VERBOSE` epilogue is the one place the session
/// states which of the two it was, so it is what this pins, in BOTH directions:
/// unset means unarmed, and `=1` means armed (a test that only checked the
/// default would pass just as well against a binary that could never arm at all).
#[cfg(unix)]
#[test]
fn the_session_model_is_off_by_default_and_arms_only_on_demand() {
    let run = |model: Option<&str>| -> String {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
        cmd.arg("--session")
            .env("SHELL", "/bin/sh")
            .env("ATERM_VERBOSE", "1") // the epilogue is the observable
            .env_remove("ATERM_CONTAINMENT_MODE")
            .env_remove("ATERM_SESSION_MODEL")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(v) = model {
            cmd.env("ATERM_SESSION_MODEL", v);
        }
        let mut child = cmd.spawn().expect("spawn the aterm CLI binary");
        child
            .stdin
            .take()
            .expect("aterm stdin")
            .write_all(b"exit\n")
            .expect("write to aterm stdin");
        String::from_utf8_lossy(&wait_with_output_bounded(child).stderr).into_owned()
    };

    let unarmed = run(None);
    assert!(
        unarmed.contains("session model off"),
        "an ordinary session must build NO VT model; stderr={unarmed:?}"
    );
    assert!(
        !unarmed.contains("ARMED"),
        "an unarmed session must not announce a model; stderr={unarmed:?}"
    );

    let armed = run(Some("1"));
    assert!(
        armed.contains("session model ARMED"),
        "$ATERM_SESSION_MODEL=1 must arm the model and say so; stderr={armed:?}"
    );
    assert!(
        armed.contains("into the armed VT core"),
        "an armed session's summary must say the bytes reached the engine; stderr={armed:?}"
    );

    // The disabling spelling is the default, not an arming: `=0` must read as OFF.
    let refused = run(Some("0"));
    assert!(
        refused.contains("session model off"),
        "$ATERM_SESSION_MODEL=0 must leave the model OFF; stderr={refused:?}"
    );
}

/// `ATERM_CONTAINMENT_MODE=containment` wraps the spawn in `sandbox-exec` (deny
/// network + credential/private-data reads). A basic shell command must STILL run
/// under the sandbox — the OS confinement must not break normal shell operation.
/// macOS-only (Seatbelt `sandbox-exec` is the actuated path).
#[cfg(target_os = "macos")]
#[test]
fn cli_runs_under_the_os_sandbox_in_containment_mode() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aterm"))
        .arg("--session") // piped stdio must still be the SESSION, not the window
        .env("SHELL", "/bin/sh")
        .env("ATERM_CONTAINMENT_MODE", "containment")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the aterm CLI binary in containment mode");
    child
        .stdin
        .take()
        .expect("aterm stdin")
        .write_all(b"echo ATERM_SANDBOXED_$((3+4))\nexit\n")
        .expect("write to aterm stdin");
    let out = wait_with_output_bounded(child);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ATERM_SANDBOXED_7"),
        "shell must run under sandbox-exec in containment mode; stdout={stdout:?}"
    );
    assert!(
        out.status.success(),
        "the sandboxed shell must still exit success; got {:?}",
        out.status
    );
}

/// Security: `ATERM_CONTAINMENT_MODE` is attacker-influenceable. A MALFORMED value
/// must FAIL CLOSED to Containment (the most restrictive mode) — never silently
/// fall through to the unconfined `User` default. The binary still spawns and runs
/// (Containment is confined, not a refusal-to-start), but in the confined mode, and
/// it announces the fallback rather than silently swallowing the garbage. Platform-
/// independent (the fallback path is the mode-parse logic; on non-macOS Containment
/// simply has no actuated OS sandbox, but the mode is still confined) — though the
/// harness drives `/bin/sh`, so it runs on POSIX hosts only.
#[cfg(unix)]
#[test]
fn malformed_containment_mode_fails_closed_not_open() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aterm"))
        .arg("--session") // piped stdio must still be the SESSION, not the window
        .env("SHELL", "/bin/sh")
        .env("ATERM_CONTAINMENT_MODE", "definitely-not-a-real-mode")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the aterm CLI binary with a malformed mode");
    child
        .stdin
        .take()
        .expect("aterm stdin")
        .write_all(b"echo ATERM_FAILCLOSED_$((5+5))\nexit\n")
        .expect("write to aterm stdin");
    let out = wait_with_output_bounded(child);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // It announced the fail-closed fallback — did NOT silently accept the garbage.
    assert!(
        stderr.contains("failing closed to Containment"),
        "a malformed mode must announce fail-closed-to-Containment; stderr={stderr:?}"
    );
    // And it still ran the shell (Containment is confined, not refuse-to-start).
    assert!(
        stdout.contains("ATERM_FAILCLOSED_10"),
        "the confined shell must still run a basic command; stdout={stdout:?}"
    );
    assert!(
        out.status.success(),
        "aterm must still exit success; got {:?}",
        out.status
    );
}
