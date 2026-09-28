// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl subscribe` rides out the replacement of the instance it is
//! subscribed to (`aterm-ctl`'s `redial` module), across REAL processes.
//!
//! THE DEFECT (code-read 2026-09-24; six self-updates in six days on the
//! owner's Mac): every update replaces the server process and its socket, and
//! `aterm ctl subscribe` relayed the hang-up as success — exit 0, "session
//! over" — while a blocking `await` failed outright. Nothing redialled.
//!
//! What this drives: instance A on an explicit (fixed) socket, a real
//! `aterm ctl subscribe` on it, A killed the way a handoff parent leaves
//! (its connections close with the process), and instance B brought up on the
//! same socket path — the successor. The subscriber must still be running, say
//! on stderr that aterm was replaced (naming both pids), and relay B's frames
//! (a second `sub` line) on the same stdout. The no-successor exit under an
//! explicit `--timeout` is driven here too (both a `subscribe` and a blocking
//! `await` exit 75 inside their deadline, not 30 s past it); the unbounded
//! no-successor wait, the flagless follow to a successor's NEW socket and the
//! negative control (the old relay's exit 0 at the hang-up) are unit-tested in
//! `aterm-ctl`'s `redial::tests`.
//!
//! ISOLATION: scratch HOME/XDG roots, a private explicit control socket, a
//! config with every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`). An instance that cannot start FAILS the
//! test (`support/headless_boot.rs` `await_ready`); SKIP only on a scratch, log
//! or spawn refusal.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

#[path = "support/headless_boot.rs"]
mod headless_boot;

use headless_boot::MAX_SOCK_PATH;

/// The scratch world, removed on drop.
struct World(PathBuf);

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A child process killed and reaped on drop, and — for an instance — the
/// lifeline that takes it down if this test process dies first.
struct Reaped(Child, Option<aterm_uds::lifeline::Lifeline>);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        // Cut after the kill, never before: the kill is what the tests time.
        drop(self.1.take());
    }
}

/// A scratch world of its own for the test tagged `tag`: the tests in this
/// binary run in parallel in one process, and a shared root would hand one
/// test's instance to the other as a "successor" on the same socket path.
fn world(tag: &str) -> Option<World> {
    let name = format!("atrd{tag}-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let root = base.join(&name);
        if root.join("run/aterm/fixed.sock.token").as_os_str().len() >= MAX_SOCK_PATH {
            continue;
        }
        if launch_isolation::prepare(&root).is_ok() {
            return Some(World(root));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    None
}

/// Boot a headless instance on the explicit socket `sock`. `None` = SKIP (a
/// log or spawn refusal); an instance that exits or never listens PANICS
/// (`headless_boot::await_ready`).
fn boot(root: &Path, sock: &Path, tag: &str) -> Option<Reaped> {
    let log_path = root.join(format!("{tag}.log"));
    let (out, err) = match std::fs::File::create(&log_path).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("SKIP: cannot open the instance log ({e})");
            return None;
        }
    };
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .arg("--control-sock")
        .arg(sock)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let lifeline = launch_isolation::lifeline(&mut cmd, root);
    let mut child = match cmd.spawn() {
        Ok(child) => Reaped(child, Some(lifeline)),
        Err(e) => {
            eprintln!("SKIP: cannot launch aterm --headless ({e})");
            return None;
        }
    };
    headless_boot::await_ready(&mut child, |r| &mut r.0, sock, &log_path, |_| true).then_some(child)
}

/// Collect a pipe's lines on a thread.
fn collect(pipe: impl std::io::Read + Send + 'static) -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    lines
}

/// Wait (bounded) until `pred` holds over the collected lines.
fn wait_for(what: &str, lines: &Arc<Mutex<Vec<String>>>, pred: impl Fn(&[String]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if pred(&lines.lock().unwrap()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {:?}",
            lines.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_subscription_follows_its_instance_across_a_replacement() {
    let Some(world) = world("r") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let root = world.0.clone();
    let sock = root.join("run/aterm/fixed.sock");

    let Some(mut a) = boot(&root, &sock, "a") else {
        return;
    };
    let a_pid = a.0.id();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, &root);
    cmd.arg("ctl")
        .arg("--sock")
        .arg(&sock)
        .args(["subscribe", "@.", "cursor,events"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut subscriber = Reaped(cmd.spawn().expect("spawn aterm ctl subscribe"), None);
    let out = collect(subscriber.0.stdout.take().unwrap());
    let err = collect(subscriber.0.stderr.take().unwrap());
    wait_for("A's stream", &out, |l| {
        l.iter().any(|x| x.starts_with("sub "))
    });

    // A goes the way a handoff parent goes: all at once, connections included.
    a.0.kill().unwrap();
    a.0.wait().unwrap();
    let Some(b) = boot(&root, &sock, "b") else {
        return;
    };
    let b_pid = b.0.id();

    wait_for("B's stream on the same stdout", &out, |l| {
        l.iter().filter(|x| x.starts_with("sub ")).count() >= 2
    });
    let note = format!("aterm was replaced (pid {a_pid} -> {b_pid}");
    wait_for("the replacement note", &err, |l| {
        l.iter().any(|x| x.contains(&note))
    });
    assert!(
        subscriber.0.try_wait().unwrap().is_none(),
        "the subscriber is still relaying — it did not exit at A's hang-up"
    );
    let sids: Vec<String> = out
        .lock()
        .unwrap()
        .iter()
        .filter(|x| x.starts_with("sub "))
        .cloned()
        .collect();
    assert_ne!(sids[0], sids[1], "B's session is B's own: {sids:?}");
    drop(b);
}

/// Wait (bounded) for `child` to exit; its exit code and when it exited.
fn exit_of(child: &mut Reaped) -> (Option<i32>, Instant) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return (status.code(), Instant::now());
        }
        assert!(Instant::now() < deadline, "the call never exited");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// An `aterm ctl` call against `sock` with piped stdout/stderr.
fn ctl(root: &Path, sock: &Path, args: &[&str]) -> Reaped {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, root);
    cmd.arg("ctl")
        .arg("--sock")
        .arg(sock)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Reaped(cmd.spawn().expect("spawn aterm ctl"), None)
}

/// An explicit `--timeout` bounds the wait for a successor too. MEASURED by the
/// review of 2026-09-24 on an isolated headless instance SIGKILLed 1 s into the
/// call: `--timeout 3 subscribe` and `--timeout 3 await match` each exited 75
/// after 31 s — the 30 s successor bound on top of the caller's deadline. Here
/// the instance is killed and nothing replaces it: both calls exit 75 (not 0,
/// not 124), well inside the 30 s the regression adds to their 10 s deadline,
/// and say the deadline is what ended the wait. The unbounded wait is the unit
/// tests' negative control (`redial::tests::an_explicit_timeout_bounds_…`).
///
/// The clock starts BEFORE two spawns of a freshly built `aterm`, whose first
/// exec macOS assesses (seconds under a loaded gate, 26 s once), while each
/// CLI's own `--timeout` starts only after its exec. So the deadline is 10 s
/// and the bound deadline + 15 s: the slack is exec latency, and the 30 s
/// regression still overshoots it by 15 s. They were 5 s and deadline + 3 s,
/// with a 4 s precondition charged two spawns (the load-sensitive test audit
/// of 2026-09-27).
#[test]
fn a_timeout_bounds_the_wait_when_nothing_replaces_the_instance() {
    let Some(world) = world("t") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let root = world.0.clone();
    let sock = root.join("run/aterm/fixed.sock");
    let Some(mut a) = boot(&root, &sock, "a") else {
        return;
    };

    // The blocking read first, so it is parked on the server by the time the
    // subscription below has its first frame.
    let started = Instant::now();
    let mut awaiter = ctl(
        &root,
        &sock,
        &[
            "--timeout",
            "10",
            "await",
            "match",
            "NEVER-7f3c",
            "timeout=60000",
        ],
    );
    let mut subscriber = ctl(
        &root,
        &sock,
        &["--timeout", "10", "subscribe", "@.", "events"],
    );
    let out = collect(subscriber.0.stdout.take().unwrap());
    let sub_err = collect(subscriber.0.stderr.take().unwrap());
    let await_err = collect(awaiter.0.stderr.take().unwrap());
    wait_for("the stream", &out, |l| {
        l.iter().any(|x| x.starts_with("sub "))
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        started.elapsed() < Duration::from_secs(9),
        "the kill must land inside the 10 s deadline for this to measure anything"
    );
    a.0.kill().unwrap();
    a.0.wait().unwrap();

    let bound = Duration::from_secs(10 + 15);
    for (what, call, err) in [
        ("subscribe", &mut subscriber, &sub_err),
        ("await", &mut awaiter, &await_err),
    ] {
        let (code, at) = exit_of(call);
        eprintln!(
            "{what}: exit {code:?} {:?} after the call started",
            at.duration_since(started)
        );
        assert_eq!(
            code,
            Some(75),
            "{what}: a server nothing replaced is exit 75: {:?}",
            err.lock().unwrap()
        );
        assert!(
            at.duration_since(started) < bound,
            "{what}: exited {:?} after the call started — the successor wait outlived \
             --timeout 10",
            at.duration_since(started)
        );
        wait_for(&format!("{what}'s note"), err, |l| {
            l.iter().any(|x| x.contains("its --timeout left"))
        });
    }
}
