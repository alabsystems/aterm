// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P0 regression test: the daily-driver CLI must run a real shell through the
//! PROTECTED spawn seam (`aterm_pty::spawn_shell`, NOT raw `forkpty`/`execvp`) and
//! stay fully functional — given a command it produces the command's OUTPUT and
//! exits with the shell's status. Complements the static guard `A6` (no
//! `libc::forkpty` in `aterm-cli/src`) with a behavioral check that the protected
//! spawn actually works end-to-end.
//!
//! It also pins that a session builds no VT model, sharing this file's
//! bounded-wait harness and the same session path.
//!
//! The unix tests drive a POSIX `/bin/sh` through the binary; the `#[cfg(windows)]`
//! twin drives the platform's default shell through the ConPTY seam. No box in
//! this fleet runs Windows natively, so `gate cells-foreign` type-checks the twin
//! on every `gate all` and it RUNS only on a Windows host.

use std::io::{Read, Write};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

/// How long the CLI gets to run the scripted shell session and exit. GENEROUS —
/// a healthy run finishes in well under a second even on a loaded CI box — because
/// the only thing this bound must catch is the exact regression these tests guard:
/// the CLI *not exiting*. `Child::wait_with_output` has no deadline and the
/// workspace gate (`cargo test`) has no per-test timeout, so without this bound a
/// doesn't-exit regression manifests as `cargo test` hanging forever instead of a
/// red test — the failure would hide inside the harness meant to detect it.
const CLI_EXIT_DEADLINE: Duration = Duration::from_secs(60);

/// THE ONE WAY THIS FILE SPAWNS `aterm --session` (2026-09-10 review). The session
/// lane reads the machine's atpkg prefix under `$HOME` and may spawn a DETACHED
/// `aterm pkg update` against it; run as-is from `cargo test`, that pass rewrote
/// the owner's REAL `<prefix>/status.toml`. Every launch therefore gets private
/// HOME/config/data roots whose `aterm.toml` switches the package and native-update
/// lanes off (`launch_isolation::CONFIG_OFF` — settings, since the environment vetoes
/// are gone), and `--no-reroute`, so nothing is laid for the upstream Rust names.
/// `/bin/sh` is the shell on unix, and piped stdio still routes to the SESSION.
fn session_command(test: &str) -> Command {
    let root = std::env::temp_dir().join(format!(
        "aterm-protected-spawn-{test}-{}",
        std::process::id()
    ));
    launch_isolation::prepare(&root).expect("prepare private session state");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, &root);
    cmd.args(["--session", launch_isolation::NO_REROUTE]);
    cmd
}

/// Bounded replacement for `Child::wait_with_output`, shared by every test in
/// this file. Dedicated threads drain the child's stdout/stderr pipes into
/// buffers (so the child can never stall on a full pipe while we wait), and the
/// main thread polls `try_wait` against `CLI_EXIT_DEADLINE`. On expiry the child
/// is killed, the drains are joined, and we panic with whatever output was
/// captured — turning a would-be infinite hang into a diagnosable failure.
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
    // No containment flag: the default User mode — no sandbox, fast.
    let mut child = session_command("protected")
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

/// A SESSION BUILDS NO VT MODEL. The daily driver once constructed a full
/// `Terminal` and fed it every PTY byte — an O(bytes) parse and O(scrollback)
/// memory for a model nothing could read — and later kept a development seam
/// (`ATERM_SESSION_MODEL`) that armed it for a consumer that never came
/// (docs/HARDCORE_BACKLOG.md §4 P0, closed 2026-09-25). aterm-cli no longer
/// links the engine at all. The `--verbose` epilogue is where the session says
/// what it did with the bytes; it names passthrough alone, and the old seam
/// set in the environment arms nothing (the negative control: before the
/// deletion, `=1` printed "session model ARMED").
#[cfg(unix)]
#[test]
fn a_session_builds_no_vt_model_and_the_old_seam_arms_nothing() {
    let run = |model: Option<&str>| -> String {
        let mut cmd = session_command("model");
        cmd.arg("--verbose") // the epilogue is the observable
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
    for model in [None, Some("1")] {
        let err = run(model);
        assert!(
            err.contains("bytes passed through."),
            "the epilogue names the passthrough ({model:?}); stderr={err:?}"
        );
        assert!(
            !err.contains("ARMED") && !err.contains("VT core"),
            "no session models the screen ({model:?}); stderr={err:?}"
        );
    }
}

/// `--containment containment` wraps the spawn in `sandbox-exec` (deny
/// network, writes outside the temp roots, and credential/private-data
/// reads). A basic shell command must STILL run
/// under the sandbox — the OS confinement must not break normal shell operation.
/// macOS-only (Seatbelt `sandbox-exec` is the actuated path).
#[cfg(target_os = "macos")]
#[test]
fn cli_runs_under_the_os_sandbox_in_containment_mode() {
    let mut child = session_command("containment")
        .args(["--containment", "containment"])
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

/// Security: the `--containment` value may be attacker-influenced. A MALFORMED value
/// must FAIL CLOSED to Containment (the most restrictive mode) — never silently
/// fall through to the unconfined `User` default — and it announces the fallback
/// rather than silently swallowing the garbage. What Containment then does is
/// the platform's: on macOS the shell runs under the OS sandbox; everywhere else
/// there is no OS sandbox, so Containment refuses to start (the owner's fail-closed
/// ruling, 2026-09-25) and the binary exits 1 naming the gap. The harness drives
/// `/bin/sh`, so it runs on POSIX hosts only.
#[cfg(unix)]
#[test]
fn malformed_containment_mode_fails_closed_not_open() {
    let mut child = session_command("malformed")
        .args(["--containment", "definitely-not-a-real-mode"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the aterm CLI binary with a malformed mode");
    // The shell may never start (off macOS), so a closed pipe is not an error.
    let _ = child
        .stdin
        .take()
        .expect("aterm stdin")
        .write_all(b"echo ATERM_FAILCLOSED_$((5+5))\nexit\n");
    let out = wait_with_output_bounded(child);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // It announced the fail-closed fallback — did NOT silently accept the garbage.
    assert!(
        stderr
            .contains("--containment takes master, user, safety or containment; using containment"),
        "a malformed mode must announce the fallback to containment; stderr={stderr:?}"
    );
    // `aterm_containment::os_sandbox_actuated()` is exactly this cfg.
    if cfg!(target_os = "macos") {
        // macOS: the confined shell runs a basic command and exits success.
        assert!(
            stdout.contains("ATERM_FAILCLOSED_10"),
            "the confined shell must still run a basic command; stdout={stdout:?}"
        );
        assert!(
            out.status.success(),
            "aterm must still exit success; got {:?}",
            out.status
        );
    } else {
        // No OS sandbox: no shell at all, exit 1, the gap named.
        assert!(
            !stdout.contains("ATERM_FAILCLOSED_10"),
            "no shell may run without the OS sandbox; stdout={stdout:?}"
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "a refused Containment exits 1; stderr={stderr:?}"
        );
        assert!(
            stderr.contains("no OS sandbox on this platform"),
            "the refusal names the platform gap; stderr={stderr:?}"
        );
    }
}

/// THE WINDOWS TWIN of `cli_runs_a_command_through_the_protected_spawn_and_exits_cleanly`:
/// the session runs the platform's default shell (`pwsh` → `powershell` →
/// `%COMSPEC%`, `aterm_pty`'s windows `shell.rs`) through the ConPTY seam, with
/// stdin a pipe (`driver_windows`' piped pump). Two echo lines, one per shell
/// family, each of which evaluates to the marker only when a real shell RAN it
/// (the ConPTY echo of the typed line shows the literal `%OS%` / `$(6*7)`), and
/// the shell's `exit 7` must come back as the session's own exit status.
#[cfg(windows)]
#[test]
fn cli_runs_a_command_through_the_conpty_seam_and_exits_with_the_shells_status() {
    let mut child = session_command("protected-win")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the aterm CLI binary");
    child
        .stdin
        .take()
        .expect("aterm stdin")
        .write_all(b"echo ATERM_P0_MARKER_%OS%\r\necho \"ATERM_P0_MARKER_$(6*7)\"\r\nexit 7\r\n")
        .expect("write to aterm stdin");
    let out = wait_with_output_bounded(child);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ATERM_P0_MARKER_Windows_NT") || stdout.contains("ATERM_P0_MARKER_42"),
        "the shell did not evaluate the command through the protected spawn; stdout={stdout:?}"
    );
    assert_eq!(
        out.status.code(),
        Some(7),
        "aterm must exit with the shell's own status; got {:?}",
        out.status
    );
}
