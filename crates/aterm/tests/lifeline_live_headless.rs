// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live headless tests for `--lifeline-fd` teardown and `aterm doctor` unreachable-instance reporting.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use crate::{child_output, ctl_accept, headless_boot, launch_isolation};

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Hang-detector ceiling for waiting on a debug binary.
const CEILING: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(20);

/// A scratch world, removed on drop.
struct World(PathBuf);

impl Drop for World {
    fn drop(&mut self) {
        headless_boot::remove_world(&self.0);
    }
}

/// Creates an isolated scratch world for `tag` in the first temp base whose socket paths fit.
fn world(tag: &str) -> World {
    let name = format!("atll{tag}-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let root = base.join(&name);
        if !launch_isolation::sock_fits(&root.join("run/aterm/aterm-4294967295.sock.token")) {
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

/// Polls until `done()` returns true or panics after [`CEILING`].
fn wait_until(what: &str, mut done: impl FnMut() -> bool, context: impl Fn() -> String) {
    let seen = aterm_spec::wait::within(CEILING, POLL, || done().then_some(()));
    assert!(
        seen.is_some(),
        "{what} did not happen within {CEILING:?}\n{}",
        context()
    );
}

fn read_log(log: &Path) -> String {
    std::fs::read_to_string(log).unwrap_or_default()
}

/// Builds a headless `aterm` command isolated under `root` logging to `log`.
fn headless(root: &Path, sock: Option<&Path>, log: &Path) -> Command {
    let out = std::fs::File::create(log).expect("the instance log");
    let err = out.try_clone().expect("the instance log, twice");
    let mut cmd = Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .args(["--lines", "24", "--columns", "100"]);
    if let Some(sock) = sock {
        cmd.arg("--control-sock").arg(sock);
    }
    cmd.stdin(Stdio::null()).stdout(out).stderr(err);
    cmd
}

/// Waits for `child` to signal readiness on `ready` or fails with `log`.
fn wait_listening(child: &mut Child, ready: &mut aterm_agent::ready::Ready, log: &Path) {
    headless_boot::await_ready(child, |c| c, ready, log, |_| true);
}

/// Runs `aterm ctl --sock <sock> <args…>` with `stdin` until served.
fn ctl(root: &Path, sock: &Path, args: &[&str], stdin: &[u8]) -> Output {
    ctl_accept::until_served(|| {
        let mut cmd = Command::new(launch_isolation::aterm_executable());
        launch_isolation::apply(&mut cmd, root);
        cmd.arg("ctl")
            .arg("--sock")
            .arg(sock)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn aterm ctl");
        child
            .stdin
            .take()
            .expect("ctl stdin")
            .write_all(stdin)
            .expect("write ctl stdin");
        child_output::wait_and_read(child, Instant::now() + Duration::from_secs(60))
            .unwrap_or_else(|| panic!("aterm ctl {args:?} did not exit in time"))
    })
}

/// A held lifeline keeps the headless instance serving, and dropping it quits cleanly and unlinks the socket.
#[test]
fn a_held_lifeline_keeps_the_instance_and_a_cut_one_ends_it_cleanly() {
    let world = world("cut");
    let root = world.0.clone();
    let sock = root.join("run/aterm/aterm.sock");
    let log = root.join("gui.log");
    let mut cmd = headless(&root, Some(&sock), &log);
    let lifeline = launch_isolation::lifeline(&mut cmd, &root);
    let mut ready = headless_boot::arm_ready(&mut cmd, &root);
    let mut instance = Started(cmd.spawn().expect("launch aterm --headless"));
    wait_listening(&mut instance.0, &mut ready, &log);

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
    assert!(
        status.success(),
        "a cut lifeline quits cleanly: {status}\n{}",
        read_log(&log)
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

/// SIGKILLing a shell launcher holding the lifeline FIFO terminates its headless instance via kernel descriptor closure.
#[test]
fn a_killed_launcher_takes_its_instance_with_it() {
    let world = world("kill");
    let root = world.0.clone();
    let sock = root.join("run/aterm/aterm.sock");
    let log = root.join("gui.log");
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
        .arg(launch_isolation::aterm_executable())
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
    std::mem::forget(guard);
}

/// `aterm doctor` reports an instance whose control socket is outside the rendezvous directory without stopping either instance.
#[test]
fn the_doctor_names_an_unreachable_instance_and_not_a_reachable_one() {
    let world_a = world("ca");
    let world_b = world("cb");
    let (root_a, root_b) = (world_a.0.clone(), world_b.0.clone());

    let sock_a = root_a.join("run/aterm/aterm.sock");
    let log_a = root_a.join("gui.log");
    let mut cmd = headless(&root_a, Some(&sock_a), &log_a);
    let _lifeline_a = launch_isolation::lifeline(&mut cmd, &root_a);
    let mut ready_a = headless_boot::arm_ready(&mut cmd, &root_a);
    let mut a = Started(cmd.spawn().expect("launch A"));
    wait_listening(&mut a.0, &mut ready_a, &log_a);

    let log_b = root_b.join("gui.log");
    let mut cmd = headless(&root_b, None, &log_b);
    let _lifeline_b = launch_isolation::lifeline(&mut cmd, &root_b);
    let mut ready_b = headless_boot::arm_ready(&mut cmd, &root_b);
    let mut b = Started(cmd.spawn().expect("launch B"));
    wait_listening(&mut b.0, &mut ready_b, &log_b);

    let mut cmd = Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, &root_b);
    let out = cmd
        .arg("doctor")
        .args(["--read-only", "--only", "base.session.instances"])
        .stdin(Stdio::null())
        .output()
        .expect("run aterm doctor");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "an unreachable instance is a note, not a failure:\n{report}"
    );
    let (pid_a, pid_b) = (a.0.id(), b.0.id());
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
    assert!(
        a.0.try_wait().expect("try_wait A").is_none()
            && b.0.try_wait().expect("try_wait B").is_none(),
        "the doctor reports; it never stops an instance"
    );
}
