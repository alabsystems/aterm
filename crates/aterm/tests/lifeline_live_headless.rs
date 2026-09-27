// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LIVE: a headless instance armed with a LIFELINE ends with the process that
//! started it, and `aterm doctor` names an instance no control socket reaches
//! (gap #36 — a `./aterm-before --headless` whose harness died had run for eleven
//! days holding a shell, reachable by no `aterm ctl ls`).
//!
//! Every instance here is the real binary (`CARGO_BIN_EXE_aterm`) under a scratch
//! world (`support/launch_isolation.rs`) and is itself armed with a lifeline, so a
//! failing assertion — or this test process being killed — cannot leak one. None
//! of these tests skips: an instance that does not boot is a failure, with its log.
//! Every wait is for its causal event (a socket listening, a reply, an exit, a pid
//! gone); the deadlines are ceilings for a broken build, never what a pass waits on.

#![cfg(unix)]

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The ceiling on any one wait. Generous: a debug binary booting on a loaded
/// machine, and a wait that is met ends the moment it is.
const CEILING: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(20);
/// Keep a unix-socket path well inside the 104-byte `sun_path`.
const MAX_SOCK_PATH: usize = 100;

/// A scratch world, removed on drop.
struct World(PathBuf);

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A private world for the test tagged `tag` (the tests in this binary run in
/// parallel): the first temp base whose socket paths fit.
fn world(tag: &str) -> World {
    let name = format!("atll{tag}-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let root = base.join(&name);
        if root
            .join("run/aterm/aterm-4294967295.sock.token")
            .as_os_str()
            .len()
            >= MAX_SOCK_PATH
        {
            continue;
        }
        let _ = std::fs::remove_dir_all(&root);
        launch_isolation::prepare(&root).expect("prepare the scratch world");
        return World(root);
    }
    panic!("no temp base gives a short enough socket path");
}

/// A process this test started, killed and reaped on drop whatever happened.
struct Started(Child);

impl Drop for Started {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `pid` still names a process (`kill(pid, 0)`: success or EPERM).
fn alive(pid: u32) -> bool {
    let pid = libc::pid_t::try_from(pid).expect("a pid");
    // SAFETY: signal 0 delivers nothing; it only asks whether `pid` exists.
    let delivered = unsafe { libc::kill(pid, 0) } == 0;
    delivered || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Wait until `done` holds, polling; panic naming `what` (and `context()`) at the
/// ceiling.
fn wait_until(what: &str, mut done: impl FnMut() -> bool, context: impl Fn() -> String) {
    let deadline = Instant::now() + CEILING;
    while !done() {
        assert!(
            Instant::now() < deadline,
            "{what} did not happen within {CEILING:?}\n{}",
            context()
        );
        std::thread::sleep(POLL);
    }
}

fn read_log(log: &Path) -> String {
    std::fs::read_to_string(log).unwrap_or_default()
}

/// The headless launch every test here makes, into `root`, its output in `log`.
/// `sock` is the explicit control socket, or `None` for the per-instance default
/// (`<root>/run/aterm/aterm-<pid>.sock`).
fn headless(root: &Path, sock: Option<&Path>, log: &Path) -> Command {
    let out = std::fs::File::create(log).expect("the instance log");
    let err = out.try_clone().expect("the instance log, twice");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .args(["--lines", "24", "--columns", "100"]);
    if let Some(sock) = sock {
        cmd.arg("--control-sock").arg(sock);
    }
    cmd.stdin(Stdio::null()).stdout(out).stderr(err);
    cmd
}

/// Wait for the instance's socket to LISTEN, failing with its log if the instance
/// exits first.
fn wait_listening(child: &mut Child, sock: &Path, log: &Path) {
    wait_until(
        "the control socket listening",
        || {
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "the instance exited ({status}) before it listened:\n{}",
                    read_log(log)
                );
            }
            launch_isolation::control_listening(sock)
        },
        || read_log(log),
    );
}

/// One `aterm ctl --sock <sock> <args…>` call, `stdin` piped in; its output.
fn ctl(root: &Path, sock: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, root);
    cmd.arg("ctl")
        .arg("--sock")
        .arg(sock)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn aterm ctl");
    // Written and closed in one statement: the client reads its stdin to the end.
    child
        .stdin
        .take()
        .expect("ctl stdin")
        .write_all(stdin)
        .expect("write ctl stdin");
    child.wait_with_output().expect("run aterm ctl")
}

/// THE CONTRACT, end to end on the real binary. While the lifeline is held the
/// instance serves — a shell answers a typed command through the socket, after
/// the whole boot. Once it is cut the instance quits the ordinary way: exit 0,
/// its socket file unlinked by the teardown (the backstop would leave it), and
/// its shell hung up.
#[test]
fn a_held_lifeline_keeps_the_instance_and_a_cut_one_ends_it_cleanly() {
    let world = world("cut");
    let root = world.0.clone();
    let sock = root.join("run/aterm/aterm.sock");
    let log = root.join("gui.log");
    let mut cmd = headless(&root, Some(&sock), &log);
    let lifeline = launch_isolation::lifeline(&mut cmd, &root);
    let mut instance = Started(cmd.spawn().expect("launch aterm --headless"));
    wait_listening(&mut instance.0, &sock, &log);
    assert!(
        read_log(&log).contains("lifeline armed on fd 0"),
        "the launch says it took the lifeline:\n{}",
        read_log(&log)
    );

    // Held: the instance runs a shell and answers through its socket.
    let sent = ctl(&root, &sock, &["send", "--stdin"], b"echo SHPID=$$\n");
    assert!(sent.status.success(), "send: {sent:?}");
    let matched = ctl(
        &root,
        &sock,
        &["await", "match", "SHPID=[0-9]+", "timeout=60000"],
        b"",
    );
    assert!(matched.status.success(), "await: {matched:?}");
    let text = ctl(&root, &sock, &["text"], b"");
    let text = String::from_utf8_lossy(&text.stdout).into_owned();
    let shell: u32 = text
        .split("SHPID=")
        .filter_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .last()
        .unwrap_or_else(|| panic!("no shell pid on the screen:\n{text}"));
    assert!(
        instance.0.try_wait().expect("try_wait").is_none(),
        "the instance ended while its lifeline was held:\n{}",
        read_log(&log)
    );
    assert!(alive(shell), "the shell {shell} is running while held");

    // Cut: the ordinary quit, not the backstop.
    let cut_at = Instant::now();
    drop(lifeline);
    let mut status = None;
    wait_until(
        "the instance exiting after its lifeline was cut",
        || {
            status = instance.0.try_wait().expect("try_wait");
            status.is_some()
        },
        || read_log(&log),
    );
    let status = status.expect("an exit status");
    let log_text = read_log(&log);
    assert!(
        status.success(),
        "a cut lifeline quits cleanly: {status}\n{log_text}"
    );
    assert!(
        log_text.contains("lifeline cut (fd 0 reached end-of-file)"),
        "{log_text}"
    );
    assert!(
        !log_text.contains("the quit has not finished"),
        "the backstop fired instead of the quit ({:?} after the cut):\n{log_text}",
        cut_at.elapsed()
    );
    assert!(
        !sock.exists(),
        "the quit's teardown unlinks the socket; the backstop would have left it"
    );
    wait_until(
        "the instance's shell hanging up",
        || !alive(shell),
        || format!("shell pid {shell}"),
    );
}

/// THE GAP'S OWN SHAPE: the launcher is a bash script (as
/// `tools/visual-judge/capture.sh` is) holding the lifeline's FIFO, and it is
/// SIGKILLed — no trap, no cleanup, no `Drop`. Its instance goes anyway, because
/// the kernel closes a dead process's descriptors and that is the end-of-file the
/// instance waits for.
///
/// The launch is the form capture.sh first had, `( exec prog <fifo 9>&- )`, on
/// purpose: bash 3.2 (macOS's `/bin/bash`) leaks a copy of fd 9 into `prog` at fd
/// 11 there (measured 2026-09-26), and an instance holding its own launcher's end
/// never read end-of-file. It passes only because the instance closes inherited
/// copies of its lifeline (`aterm_uds::lifeline::close_inherited_copies`).
#[test]
fn a_killed_launcher_takes_its_instance_with_it() {
    let world = world("kill");
    let root = world.0.clone();
    let sock = root.join("run/aterm/aterm.sock");
    let log = root.join("gui.log");
    // The script holds the FIFO read-write on fd 9 (the one writer), the instance
    // reads it as stdin, and the script prints the instance's pid and waits on it.
    // The FIFO's name stays until the world is removed: the subshell opens it
    // AFTER the fork, so an `rm` here would race that open and could win.
    let script = r#"
        mkfifo "$1/lifeline" || exit 1
        exec 9<>"$1/lifeline"
        ( exec "$2" --headless --no-reroute --lifeline-fd 0 --control-sock "$3" \
          <"$1/lifeline" 9>&- ) >"$4" 2>&1 &
        echo "$!"
        wait
    "#;
    let shell = ["/bin/bash", "/bin/sh"]
        .into_iter()
        .find(|s| Path::new(s).exists())
        .expect("a shell");
    let mut cmd = Command::new(shell);
    launch_isolation::apply(&mut cmd, &root);
    cmd.arg("-c")
        .arg(script)
        .arg("launcher")
        .arg(&root)
        .arg(env!("CARGO_BIN_EXE_aterm"))
        .arg(&sock)
        .arg(&log)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut launcher = Started(cmd.spawn().expect("spawn the launcher script"));
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(launcher.0.stdout.take().expect("launcher stdout")),
        &mut line,
    )
    .expect("the launcher prints its instance's pid");
    let pid: u32 = line
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("not a pid: {line:?}"));
    // Whatever the assertions below find, the instance this test started ends.
    struct KillOnDrop(u32);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            if let Ok(pid) = libc::pid_t::try_from(self.0) {
                // SAFETY: SIGKILL to the one pid this test started.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
    let guard = KillOnDrop(pid);
    wait_until(
        "the scripted instance listening",
        || launch_isolation::control_listening(&sock),
        || read_log(&log),
    );
    assert!(alive(pid), "the instance runs while its launcher lives");

    launcher.0.kill().expect("SIGKILL the launcher");
    launcher.0.wait().expect("reap the launcher");
    wait_until(
        "the instance ending after its launcher was SIGKILLed",
        || !alive(pid),
        || read_log(&log),
    );
    // Gone, and reaped by launchd: the number is free for any process now, and a
    // SIGKILL sent to it later could land on one this test never started.
    std::mem::forget(guard);
    assert!(
        read_log(&log).contains("lifeline cut (fd 0 reached end-of-file)"),
        "{}",
        read_log(&log)
    );
}

/// THE CENSUS on real processes: `aterm doctor`, run in B's world, names A — whose
/// socket lives where B's rendezvous directory never looks — and not B, whose
/// per-instance socket it publishes. Both are reported, neither is touched: each
/// is still running after the doctor has spoken.
#[test]
fn the_doctor_names_an_unreachable_instance_and_not_a_reachable_one() {
    let world_a = world("ca");
    let world_b = world("cb");
    let (root_a, root_b) = (world_a.0.clone(), world_b.0.clone());

    let sock_a = root_a.join("run/aterm/aterm.sock");
    let log_a = root_a.join("gui.log");
    let mut cmd = headless(&root_a, Some(&sock_a), &log_a);
    let _lifeline_a = launch_isolation::lifeline(&mut cmd, &root_a);
    let mut a = Started(cmd.spawn().expect("launch A"));
    wait_listening(&mut a.0, &sock_a, &log_a);

    let log_b = root_b.join("gui.log");
    let mut cmd = headless(&root_b, None, &log_b);
    let _lifeline_b = launch_isolation::lifeline(&mut cmd, &root_b);
    let mut b = Started(cmd.spawn().expect("launch B"));
    let sock_b = root_b.join(format!("run/aterm/aterm-{}.sock", b.0.id()));
    wait_listening(&mut b.0, &sock_b, &log_b);

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, &root_b);
    let out = cmd
        .arg("doctor")
        .stdin(Stdio::null())
        .output()
        .expect("run aterm doctor");
    let report = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "an unreachable instance is a note, not a failure:\n{report}"
    );
    let (pid_a, pid_b) = (a.0.id(), b.0.id());
    // Each unreached instance is a line of its own under the row's fact column
    // (doctor prints one row per check: label, mark, fact).
    assert!(
        report.lines().any(|l| l.starts_with(' ')
            && l.trim_start().starts_with(&format!("pid {pid_a} "))
            && l.contains("--headless")),
        "A (pid {pid_a}) is named:\n{report}"
    );
    assert!(
        !report.contains(&format!("pid {pid_b} ")),
        "B (pid {pid_b}) is published in the directory the doctor read:\n{report}"
    );
    let dir_b = root_b.join("run/aterm");
    assert!(
        report.contains(&format!(
            "reachable by no control socket in {}",
            dir_b.display()
        )),
        "the row names the directory it read:\n{report}"
    );
    assert!(report.contains("never stopped"), "{report}");
    assert!(
        a.0.try_wait().expect("try_wait A").is_none()
            && b.0.try_wait().expect("try_wait B").is_none(),
        "the doctor reports; it never stops an instance"
    );
}
