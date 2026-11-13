// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use std::path::PathBuf;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

/// Set, the test binary is [`standin_waits_on_a_watch`]: it writes its pid into this
/// directory's `waiting` once its watch is live.
const STANDIN: &str = "ATERM_TOOLCHAIN_INTERRUPT_STANDIN";
/// Set, the stand-in first puts SIGINT back to its default action, as a foreground process
/// has it; else it keeps the disposition it was started with.
const STANDIN_SIGINT: &str = "ATERM_TOOLCHAIN_INTERRUPT_SIGINT";
/// Set, the stand-in ignores SIGINT before its watch begins, as a background job of a
/// non-interactive shell starts.
const STANDIN_IGNORE: &str = "ATERM_TOOLCHAIN_INTERRUPT_IGNORE";
/// Set, a second SIGTERM reaches the stand-in after its wait took the first and before its
/// watch ends: it sends it to itself there, holding that point of the race.
const STANDIN_TWICE: &str = "ATERM_TOOLCHAIN_INTERRUPT_TWICE";
/// Set, the stand-in first catches SIGQUIT in a handler of its own that exits 4, as a tool's
/// own handling, so a SIGQUIT no watch notes ends it with no core.
const STANDIN_QUIT: &str = "ATERM_TOOLCHAIN_INTERRUPT_QUIT";

/// [`STANDIN_QUIT`]'s handler.
extern "C" fn quit_exits_4(_: libc::c_int) {
    // SAFETY: `_exit` is async-signal-safe and ends the process at once.
    unsafe { libc::_exit(4) }
}

/// Run by the tests below in a process of its own: naps on a watch for twice its parent's
/// one-minute hang detector, so only an interrupt the watch takes ends it in time, then
/// exits 3, as a tool's NOT RUN, naming the signal.
#[test]
fn standin_waits_on_a_watch() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    // SAFETY: SIG_DFL and SIG_IGN are dispositions, no handler of ours.
    unsafe {
        // SIGTERM at its default action, whatever this test binary was started with: a
        // watch leaves an ignored one ignored, and the SIGTERM legs send it.
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        if std::env::var_os(STANDIN_SIGINT).is_some() {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
        }
        if std::env::var_os(STANDIN_IGNORE).is_some() {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
        }
        if std::env::var_os(STANDIN_QUIT).is_some() {
            libc::signal(
                libc::SIGQUIT,
                quit_exits_4 as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }
    aterm_uds::signal_origin::observe(|line| println!("{line}"));
    let watch = Watch::enter();
    std::fs::write(dir.join("waiting"), std::process::id().to_string()).expect("said");
    assert!(
        watch.nap(MUST_HAPPEN * 2, &|| false),
        "no interrupt ended the watch"
    );
    let sig = watch.take().expect("an interrupt ended the nap");
    let twice = std::env::var_os(STANDIN_TWICE).is_some();
    if twice {
        resend(libc::SIGTERM);
        until_within(MUST_HAPPEN, "the second SIGTERM noted", || watch.caught());
    }
    drop(watch);
    if twice {
        // The second SIGTERM, sent again at its default action, ends the process while it
        // naps (a process-directed signal reaches some thread soon after, not at once); a
        // lost one leaves it to its parent's hang detector.
        std::thread::sleep(MUST_HAPPEN * 2);
    }
    println!("stopped by {}", name(sig));
    std::process::exit(3);
}

/// A stand-in waiting on a watch, its pid, and its scratch directory. One still running
/// when it is dropped (a test that failed before it ended) is killed, so none outlives its
/// test.
struct Standin {
    child: aterm_uds::spawnfd::Child,
    pid: i32,
    dir: aterm_tempfile::TempDir,
}

impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Start [`standin_waits_on_a_watch`] with `env` set, once its watch is live.
fn standin(env: &[&str]) -> Standin {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atc-interrupt-")
        .tempdir()
        .expect("a scratch dir");
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    for key in env {
        cmd.env(key, "1");
    }
    let child = cmd
        .args([
            "--exact",
            "interrupt::tests::standin_waits_on_a_watch",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(STANDIN, dir.path())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in");
    let mut standin = Standin { child, pid: 0, dir };
    let waiting = standin.dir.path().join("waiting");
    standin.pid = until_within(MUST_HAPPEN, "the stand-in's watch", || {
        std::fs::read_to_string(&waiting).ok()?.trim().parse().ok()
    });
    standin
}

/// Send `sig` to the stand-in and read how it ended, within its one-minute hang detector.
fn signal(standin: &mut Standin, sig: libc::c_int) -> (std::process::ExitStatus, String, Duration) {
    let sent = Instant::now();
    // SAFETY: `kill` takes two integers; the pid is this test's own child.
    assert_eq!(
        unsafe { libc::kill(standin.pid, sig) },
        0,
        "signalled the stand-in"
    );
    let status = until_within(MUST_HAPPEN, "the stand-in's end", || {
        standin.child.try_wait().ok().flatten()
    });
    let took = sent.elapsed();
    let mut out = String::new();
    if let Some(mut o) = standin.child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut o, &mut out);
    }
    (status, out, took)
}

/// SIGTERM, SIGINT where the process does not ignore it, and SIGQUIT end a wait as NOT RUN
/// well inside the one-minute hang detector (the time is reported), the wait naming the
/// signal; the process's own handling would have ended it by the signal instead (default
/// action), or, for SIGQUIT, by its own handler's exit 4 (no core is dumped either way).
#[test]
fn an_interrupt_ends_a_watched_wait() {
    for (env, sig, word) in [
        (&[][..], libc::SIGTERM, "SIGTERM"),
        (&[STANDIN_SIGINT][..], libc::SIGINT, "SIGINT"),
        (&[STANDIN_QUIT][..], libc::SIGQUIT, "SIGQUIT"),
    ] {
        let mut standin = standin(env);
        let (status, out, took) = signal(&mut standin, sig);
        assert_eq!(status.code(), Some(3), "{word}: {status} {out}");
        assert!(out.contains(&format!("stopped by {word}")), "{out}");
        assert!(
            out.contains(&format!("sender_pid={} ", std::process::id())),
            "{out}"
        );
        assert!(out.contains("executable=Some("), "{out}");
        assert!(out.contains("argv=Some("), "{out}");
        eprintln!("{word} ended the stand-in's wait in {took:?}");
    }
}

/// An ignored SIGINT stays ignored through a watch: SIGINT, then SIGTERM, end the wait on
/// SIGTERM. A watch notes only the first signal it catches, and the kernel delivers SIGINT
/// no later than SIGTERM, so a caught SIGINT would have been the one named.
#[test]
fn an_ignored_interrupt_stays_ignored() {
    let mut standin = standin(&[STANDIN_IGNORE]);
    // SAFETY: as in `signal`.
    assert_eq!(unsafe { libc::kill(standin.pid, libc::SIGINT) }, 0);
    let (status, out, _) = signal(&mut standin, libc::SIGTERM);
    assert_eq!(status.code(), Some(3), "{status} {out}");
    assert!(out.contains("stopped by SIGTERM"), "{out}");
}

/// A second interrupt that comes after a wait took the first, before its watch ends, is no
/// wait's: it is sent again once the process's own handling is back, which ends the
/// process at once (here the default action, killed by SIGTERM), not after the NOT RUN the
/// first one led to. Were a taken signal's mark kept instead, the second would be lost and
/// the stand-in would exit 3.
#[test]
fn a_second_interrupt_after_the_wait_took_one_is_not_lost() {
    use std::os::unix::process::ExitStatusExt as _;
    let mut standin = standin(&[STANDIN_TWICE]);
    let (status, out, _) = signal(&mut standin, libc::SIGTERM);
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status} {out}");
    assert!(!out.contains("stopped by"), "{out}");
}

#[test]
fn an_unbounded_nap_does_not_overflow_instant() {
    let watch = Watch::enter();
    assert!(watch.nap(Duration::MAX, &|| true));
}
