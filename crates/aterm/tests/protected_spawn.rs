// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that `aterm --session` runs a shell through the protected spawn seam and exits cleanly.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::{Read, Write};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::launch_isolation;

const CLI_EXIT_DEADLINE: Duration = Duration::from_secs(60);

fn session_command(test: &str) -> (Command, aterm_tempfile::TempDir) {
    let root = launch_isolation::scratch_root(&format!("aterm-protected-spawn-{test}"));
    let mut cmd = Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root.path());
    cmd.args(["--session", launch_isolation::NO_REROUTE]);
    (cmd, root)
}

fn wait_with_output_bounded(mut child: Child) -> Output {
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
                // The child is gone, so its pipe write-ends are closed and the drains reach EOF
                // (the grandchild shell lives on the PTY slave, not these pipes).
                return Output {
                    status,
                    stdout: stdout.join().expect("join the stdout drain"),
                    stderr: stderr.join().expect("join the stderr drain"),
                };
            }
            None if Instant::now() >= deadline => {
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
                    "CLI did not exit within {}s; captured stdout={out:?} stderr={err:?}",
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
    let (mut cmd, _root) = session_command("protected");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the aterm CLI binary");

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

/// `--containment containment` wraps the spawn in `sandbox-exec` (deny network, other
/// processes' task ports, writes outside the temp roots, and credential/private-data reads),
/// and a basic shell command still runs under it. macOS only (Seatbelt is the actuated path).
#[cfg(target_os = "macos")]
#[test]
fn cli_runs_under_the_os_sandbox_in_containment_mode() {
    let (mut cmd, _root) = session_command("containment");
    let mut child = cmd
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

/// The `--containment` value may be attacker-influenced: a malformed value fails closed to
/// Containment (never the unconfined `User` default) and announces the fallback. On macOS the
/// shell then runs under the OS sandbox; elsewhere there is none, so Containment refuses to
/// start and the binary exits 1 naming the gap. The harness drives `/bin/sh`: POSIX only.
#[cfg(unix)]
#[test]
fn malformed_containment_mode_fails_closed_not_open() {
    let (mut cmd, _root) = session_command("malformed");
    let mut child = cmd
        .args(["--containment", "definitely-not-a-real-mode"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the aterm CLI binary with a malformed mode");
    let _ = child
        .stdin
        .take()
        .expect("aterm stdin")
        .write_all(b"echo ATERM_FAILCLOSED_$((5+5))\nexit\n");
    let out = wait_with_output_bounded(child);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("using containment"),
        "a malformed mode must announce the fallback to containment; stderr={stderr:?}"
    );
    if cfg!(target_os = "macos") {
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
        assert!(
            !stdout.contains("ATERM_FAILCLOSED_10"),
            "no shell may run without the OS sandbox; stdout={stdout:?}"
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "a refused Containment exits 1; stderr={stderr:?}"
        );
    }
}

/// The Windows twin of `cli_runs_a_command_through_the_protected_spawn_and_exits_cleanly`:
/// the session runs the platform's default shell (`pwsh` → `powershell` → `%COMSPEC%`,
/// `aterm_pty`'s windows `shell.rs`) through ConPTY with stdin a pipe. Two echo lines, one
/// per shell family, each evaluate to the marker only when a real shell ran them (the ConPTY
/// echo shows the literal `%OS%` / `$(6*7)`), and the shell's `exit 7` is the session's exit
/// status.
#[cfg(windows)]
#[test]
fn cli_runs_a_command_through_the_conpty_seam_and_exits_with_the_shells_status() {
    let (mut cmd, _root) = session_command("protected-win");
    let mut child = cmd
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
