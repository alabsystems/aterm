// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The front door must route every VERB before the MODE FORK.
//!
//! `aterm ship` shipped wired only into the WINDOW library's parser
//! (`aterm-gui`'s `parse_cli`). An operator at a terminal never reaches that
//! parser: with a TTY on stdin the mode fork in `main` selects the SESSION,
//! whose parser knows no `ship` and rejected it as an unknown option. The verb
//! worked when stdin was a PIPE — CI, a `| head`, a captured run — and failed at
//! a prompt, which is the only place anyone types it. Every automated check
//! passed while the one person the verb was written for could not run it.
//!
//! So these tests drive the real binary under a REAL pty. Piped stdio takes the
//! window route and would pass against the broken build; only a terminal on
//! stdin exercises the branch that was wrong.

#![cfg(unix)]

use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a verb gets to answer. GENEROUS — `--help` returns in milliseconds —
/// because the only failure this bound must catch is the verb NOT EXITING, and
/// `cargo test` has no per-test timeout: without it, a hang regression would
/// manifest as the whole suite wedging instead of one red test.
const DEADLINE: Duration = Duration::from_secs(60);

/// Run `aterm <args…>` with a REAL terminal on all three streams and return
/// everything it wrote. This is the operator's situation, and the one the
/// regression depended on.
fn on_a_terminal(args: &[&str]) -> String {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: `openpty` writes the two fds through its out-params; the trailing
    // three are NULL, which the API defines as "default termios and window size".
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
    // SAFETY: both fds come from the successful `openpty` above and are owned
    // here — nothing else holds or closes them.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };

    let mut child = Command::new(env!("CARGO_BIN_EXE_aterm"))
        .args(args)
        // PRESENCE of this variable forces the window route on its own, which
        // would silently restore the very bypass this test exists to forbid.
        .env_remove("ATERM_HEADLESS")
        .stdin(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .stdout(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .stderr(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .spawn()
        .expect("spawn the aterm binary");
    // The parent's copy must go, or the master never sees EOF: the read below
    // would block on a slave end nobody is writing to.
    drop(slave);

    // Drain on its own thread so the child can never stall against a full pty
    // buffer while the main thread waits for it to exit.
    let mut pty = std::fs::File::from(master);
    let drain = std::thread::spawn(move || {
        let (mut out, mut chunk) = (Vec::new(), [0u8; 4096]);
        loop {
            match pty.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                // EIO on the master is how a pty reports "the last slave closed".
                Err(_) => break,
            }
        }
        out
    });

    let start = Instant::now();
    loop {
        match child.try_wait().expect("wait on aterm") {
            Some(_) => break,
            None if start.elapsed() >= DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`aterm {}` did not exit within {DEADLINE:?}",
                    args.join(" ")
                );
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    // A pty writes CRLF; strip the CR so callers match on plain text.
    String::from_utf8_lossy(&drain.join().expect("drain the pty")).replace('\r', "")
}

/// The same invocation with stdio PIPED — the route the broken build happened to
/// get right, kept here only so the parity test below can compare the two.
fn through_a_pipe(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_aterm"))
        .args(args)
        .env_remove("ATERM_HEADLESS")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run the aterm binary");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text
}

/// THE GATE, over the WHOLE roster.
///
/// Every verb must be routed by the front door at a REAL terminal. The probe is a
/// flag no tool defines, so each verb answers in its OWN voice — a usage error from
/// the tool it dispatches to — and the assertion is simply that the answer is not
/// the SESSION parser's. That is the exact failure `ship` produced, and it is the
/// failure any future verb wired below the mode fork will produce.
///
/// `aterm_cli::Verb::ALL` drives the loop, so a verb added tomorrow is covered the
/// moment it joins the roster; nobody has to remember this file.
#[test]
fn every_verb_is_routed_by_the_front_door_at_a_terminal() {
    // A flag no verb's tool defines: it provokes each tool's own usage error
    // without asking any of them to DO anything.
    const PROBE: &str = "--aterm-front-door-routing-probe";
    for verb in aterm_cli::Verb::ALL {
        let out = on_a_terminal(&[verb.name(), PROBE]);
        assert!(
            !out.contains(&format!("aterm: unknown option {}", verb.name())),
            "`aterm {}` fell through to the SESSION parser at a terminal — the verb is \
             routed below the mode fork, not by the front door; output was {out:?}",
            verb.name()
        );
    }
}

/// The GENERAL invariant the regression violated: a verb is a verb whether or not
/// stdin is a terminal. Routing that happens after the mode fork cannot satisfy
/// this, so this fails the moment a verb is wired into one mode's parser.
#[test]
fn every_verb_answers_the_same_with_or_without_a_terminal() {
    const PROBE: &str = "--aterm-front-door-routing-probe";
    let first = |s: &str| {
        s.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    for verb in aterm_cli::Verb::ALL {
        let tty = on_a_terminal(&[verb.name(), PROBE]);
        let pipe = through_a_pipe(&[verb.name(), PROBE]);
        assert_eq!(
            first(&tty),
            first(&pipe),
            "`aterm {}` answered differently at a terminal than through a pipe: the \
             verb is being routed by the MODE FORK rather than by the front door",
            verb.name()
        );
    }
}

/// THE NAMED REGRESSION. `aterm ship` at a prompt must reach the release tool, not the
/// session's argument parser.
///
/// `--help` is the payload deliberately: it proves the verb dispatched and the
/// arguments after it were forwarded verbatim, while publishing nothing.
#[test]
fn ship_is_a_front_door_verb_at_a_terminal() {
    let out = on_a_terminal(&["ship", "--help"]);
    assert!(
        !out.contains("unknown option"),
        "`aterm ship` fell through to a parser that does not know the verb — the \
         exact regression this test exists for; output was {out:?}"
    );
    // Either outcome proves dispatch: the tool ran, or `run_ship` reported that
    // this machine does not carry it. Both are the verb's own voice.
    assert!(
        out.contains("aterm-release") || out.contains("not set up to publish"),
        "`aterm ship` must reach the release tool at a terminal; output was {out:?}"
    );
}
