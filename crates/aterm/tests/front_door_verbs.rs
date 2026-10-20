// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The front door routes every verb before the mode fork. With a TTY on stdin the mode fork
//! selects the session, whose parser rejects a verb it does not know, so these tests drive the
//! real binary under a real pty: piped stdio takes the window route and would hide a verb wired
//! into only one mode's parser.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

/// One private HOME/XDG/TMPDIR root for every launch of one test (the parity check's pair
/// included), removed when its guard drops. Without it a verb that dials (`aterm ctl …`) would
/// reach the live aterm on the machine running the suite.
fn private_root() -> aterm_tempfile::TempDir {
    launch_isolation::scratch_root("aterm-front-door")
}

/// `cmd` launched in `root` ([`private_root`]).
fn isolated<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    launch_isolation::apply(cmd, root);
    cmd
}

/// How long a verb gets to answer: generous (`--help` returns in milliseconds) because the one
/// failure it catches is the verb not exiting, and `cargo test` has no per-test timeout.
const DEADLINE: Duration = Duration::from_secs(60);

/// Run `aterm <args…>` with a real terminal on all three streams and return everything it
/// wrote: an operator's situation.
fn on_a_terminal(root: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    isolated(&mut cmd, root).args(args);
    capture_on_a_terminal(cmd)
}

/// Run `cmd` with a real terminal on all three streams and return everything it wrote, CR
/// stripped.
///
/// The parent holds its slave end until it has read the master dry: on macOS, output a child
/// wrote is discarded when the last slave descriptor closes before the master has read it
/// (measured: `/bin/echo` under a pty, the master read after the child exited, 20 of 20
/// outputs lost; 0 of 20 with the slave held). With the slave held the master never sees EOF,
/// so it is read without blocking while the child runs (the child never stalls on a full
/// buffer) and once more after its exit. What a descendant writes after the direct child
/// exited is not waited for.
fn capture_on_a_terminal(mut cmd: Command) -> String {
    // Both ends close-on-exec from birth: the child holds its terminal and never
    // a copy of its master, and no sibling test's child inherits either end.
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both fds come from the successful open above and are owned
    // here — nothing else holds or closes them.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    aterm_pty::set_nonblocking(master.as_raw_fd(), true).expect("make the pty master non-blocking");

    let what = format!("{cmd:?}");
    let mut child = cmd
        .stdin(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .stdout(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .stderr(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .spawn()
        .expect("spawn the child under a pty");
    // A `Command` keeps the descriptors it was given until it is dropped: drop it now, so
    // `slave` below is the one copy this process holds, held on purpose.
    drop(cmd);

    let mut pty = std::fs::File::from(master);
    let mut out = Vec::new();
    let start = Instant::now();
    loop {
        read_available(&mut pty, &mut out, start);
        match child.try_wait().expect("wait on the child") {
            Some(_) => break,
            None if start.elapsed() >= DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`{what}` did not exit within {DEADLINE:?}");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    // Everything the child wrote is in the terminal's queue, kept there by the
    // slave this process still holds.
    read_available(&mut pty, &mut out, start);
    drop(slave);
    // A pty writes CRLF; strip the CR so callers match on plain text.
    String::from_utf8_lossy(&out).replace('\r', "")
}

/// Read what the non-blocking pty master holds now, appending it to `out`. Stops
/// at the deadline too, so a child that floods the terminal can never starve the
/// caller's hang detector.
fn read_available(pty: &mut std::fs::File, out: &mut Vec<u8>, start: Instant) {
    let mut chunk = [0u8; 4096];
    while start.elapsed() < DEADLINE {
        match pty.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // Read dry for now. The slave is held, so nothing else ends a read.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(e) => panic!("reading the pty master failed: {e}"),
        }
    }
}

/// The same invocation with stdio piped, for the parity check.
fn through_a_pipe(root: &Path, args: &[&str]) -> String {
    let out = isolated(&mut Command::new(env!("CARGO_BIN_EXE_aterm")), root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run the aterm binary");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text
}

/// Every verb is routed by the front door at a real terminal. The probe is a flag no tool
/// defines, so each verb answers in its own voice (its tool's usage error), and the answer must
/// not be the session parser's. A verb is a verb whether or not stdin is a terminal, so its
/// first line at a terminal is its first line through a pipe. `aterm_cli::Verb::ALL` drives the
/// loop, so a new verb is covered when it joins the roster.
#[test]
fn every_verb_is_routed_by_the_front_door_at_a_terminal() {
    // A flag no verb's tool defines: it provokes each tool's own usage error without asking
    // any of them to do anything.
    const PROBE: &str = "--aterm-front-door-routing-probe";
    let first = |s: &str| {
        s.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let root = private_root();
    for verb in aterm_cli::Verb::ALL {
        let out = on_a_terminal(root.path(), &[verb.name(), PROBE]);
        // An answer that says nothing proves nothing: an empty terminal
        // capture would pass the check below whatever parser ran.
        assert!(
            !out.trim().is_empty(),
            "`aterm {} {PROBE}` wrote nothing at a terminal",
            verb.name()
        );
        assert!(
            !out.contains(&format!("aterm: unknown command {}", verb.name())),
            "`aterm {}` fell through to the SESSION parser at a terminal — the verb is \
             routed below the mode fork, not by the front door; output was {out:?}",
            verb.name()
        );
        let pipe = through_a_pipe(root.path(), &[verb.name(), PROBE]);
        assert_eq!(
            first(&out),
            first(&pipe),
            "`aterm {}` answered differently at a terminal than through a pipe",
            verb.name()
        );
    }
}

/// A roster tool that is not installed says so, at a terminal and through a pipe —
/// never a parser's "unknown command".
#[test]
fn an_uninstalled_roster_tool_names_the_install_not_an_unknown_command() {
    let root = private_root();
    for out in [
        on_a_terminal(root.path(), &["ty"]),
        through_a_pipe(root.path(), &["ty"]),
    ] {
        assert!(out.contains("aterm pkg install ty"), "{out:?}");
        assert!(!out.contains("unknown option"), "{out:?}");
        assert!(!out.contains("unknown command"), "{out:?}");
    }
    // A command a program ships (`targo` comes with `trust`), which `aterm --help`
    // names beside the programs: its program's install, never "unknown command".
    for out in [
        on_a_terminal(root.path(), &["targo", "build"]),
        through_a_pipe(root.path(), &["targo", "build"]),
    ] {
        assert!(
            out.contains(
                "targo comes with trust, which is not installed (fix: aterm pkg install trust)"
            ),
            "{out:?}"
        );
        assert!(!out.contains("unknown command"), "{out:?}");
    }
}

/// An answer into a reader that has already gone (`| head -1`, `| grep -q`) ends
/// there: exit 0 and no panic. std's `print!` panicked on that EPIPE ("failed
/// printing to stdout: Broken pipe", exit 101) — every one of these verbs did,
/// deterministically, once the reader closed before the first write; only a long
/// answer did under a `| head -1` that closed mid-way. The verbs a library answers
/// in-process (drive, fleet, link, fabric, keeper, caffeine) print through the same writer
/// (`aterm_uds::stdout`) as the front door's own.
#[test]
fn an_answer_into_a_closed_pipe_exits_cleanly() {
    let root = private_root();
    for args in [
        &["--help"][..],
        &["--version"],
        &["help", "ctl"],
        &["list-kitty-commands"],
        &["theme", "list"],
        &["agents", "primer"],
        &["pkg", "--help"],
        &["update", "--help"],
        &["drive", "--help"],
        &["drive", "help"],
        &["drive"],
        &["drive", "classify", "ls"],
        &["fleet", "--help"],
        &["fleet"],
        &["link", "--help"],
        &["fabric", "--help"],
        &["keeper", "--help"],
        &["caffeine", "--help"],
    ] {
        let (reader, writer) = std::io::pipe().expect("a pipe");
        drop(reader);
        let out = isolated(&mut Command::new(env!("CARGO_BIN_EXE_aterm")), root.path())
            .args(args)
            .stdin(Stdio::null())
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .expect("run the aterm binary");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("panicked"),
            "`aterm {}` panicked into a closed pipe: {stderr}",
            args.join(" ")
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "`aterm {}` into a closed pipe: {stderr}",
            args.join(" ")
        );
    }
}

/// An answer a full disk refused (`> /dev/full`, ENOSPC) is said on stderr and exits 1, never
/// the verb's success over an empty file: a script that runs `aterm drive … > out` must see the
/// answer was lost. Only a reader that went away ends an answer quietly
/// ([`an_answer_into_a_closed_pipe_exits_cleanly`]); the verbs a library answers in-process
/// exited 0 here and wrote nothing. Linux, whose /dev/full is that disk.
#[cfg(target_os = "linux")]
#[test]
fn an_answer_a_full_disk_refused_exits_1_and_says_so() {
    let root = private_root();
    for args in [
        &["--help"][..],
        &["drive", "--help"],
        &["drive", "classify", "ls"],
        &["drive", "classify", "rm -rf build"],
        &["fleet", "--help"],
        &["link", "--help"],
        &["fabric", "--help"],
        &["keeper", "--help"],
        &["caffeine", "--help"],
    ] {
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("open /dev/full");
        let out = isolated(&mut Command::new(env!("CARGO_BIN_EXE_aterm")), root.path())
            .args(args)
            .stdin(Stdio::null())
            .stdout(full)
            .stderr(Stdio::piped())
            .output()
            .expect("run the aterm binary");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(1),
            "`aterm {}` into a full disk: {stderr}",
            args.join(" ")
        );
        assert!(
            stderr.contains("the answer was not written ("),
            "`aterm {}` says the answer was lost: {stderr}",
            args.join(" ")
        );
    }
}

/// `aterm drive` reaches the control client of the SAME executable (`ctl` is one
/// of its verbs, and `aterm-ctl` its argv0 alias; `drive phase` dials it, `drive
/// read` having left with the other driving verbs), with no alias beside it and
/// none on PATH: a plain `target/debug/aterm` answered "could not run aterm-ctl:
/// No such file or directory", and with an older install's `aterm-ctl` on PATH it
/// drove that one instead.
#[test]
fn drive_reaches_this_images_own_control_client() {
    let root = private_root();
    let sock = root.path().join("nobody-serves.sock");
    let out = isolated(&mut Command::new(env!("CARGO_BIN_EXE_aterm")), root.path())
        .args(["drive", "--sock"])
        .arg(&sock)
        .args(["phase", "@s-1"])
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .expect("run the aterm binary");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(!text.contains("could not run"), "{text}");
    assert!(
        text.contains(&format!("aterm-ctl: connect {}", sock.display())),
        "the image's own client dialled the socket: {text}"
    );
}

/// A refused `--sandbox` (no OS sandbox off macOS) at a terminal says only that
/// no shell was started: the session banner waits for a shell that HAS spawned.
/// It used to print "session started" first, then the refusal.
#[cfg(not(target_os = "macos"))]
#[test]
fn a_refused_sandbox_never_says_the_session_started() {
    let root = private_root();
    let out = on_a_terminal(root.path(), &["--sandbox"]);
    assert!(out.contains("the sandbox exists only on macOS"), "{out:?}");
    assert!(!out.contains("session started"), "{out:?}");
}
