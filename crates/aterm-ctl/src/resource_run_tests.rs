// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real self-registration and exec controls. Only children receive fixture
//! environment; the test harness never changes its own process environment.

use super::*;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const FIXTURE: &str = "ATERM_RESOURCE_RUN_FIXTURE";
const TEST: &str = "conn::resource_run::process_tests::registered_exec_fixture";
const HANG: Duration = Duration::from_secs(60);

#[test]
fn registered_exec_fixture() {
    let Ok(spec) = std::env::var(FIXTURE) else {
        return;
    };
    let (sock, mode) = spec.split_once('\n').expect("fixture spec");
    let script = match mode {
        "exit" => {
            r#"printf 'RUN pid=%s stale=%s owner=%s arg=<%s>\n' "$$" "$ATERM_SESSION_ID" "$ATERM_PARENT_SESSION_ID" "$1"; exit 37"#
        }
        "fds" => r#"printf 'first-descriptor\n' >&3; printf 'second-descriptor\n' >&4; exit 37"#,
        "signal" => {
            r#"trap 'printf "SIGNAL\n"; read release; exit 37' HUP INT TERM; printf 'READY pid=%s\n' "$$"; while :; do read request; done"#
        }
        _ => panic!("unknown fixture"),
    };
    let args = [
        "--sock",
        sock,
        "--session",
        "@self",
        "--job",
        "tree:program",
        "--",
        "/bin/sh",
        "-c",
        script,
        "sh",
        "",
    ]
    .map(OsString::from)
    .to_vec();
    std::process::exit(if run_registered_command(args).is_ok() {
        0
    } else {
        1
    });
}

struct Host {
    _dir: aterm_tempfile::TempDir,
    sock: std::path::PathBuf,
    peer: mpsc::Receiver<u32>,
    done: std::thread::JoinHandle<()>,
}

impl Host {
    fn new(refuse: bool) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("at-resource-")
            .tempdir_in("/tmp")
            .unwrap();
        let sock = dir.path().join("s");
        let listener = aterm_uds::CtlListener::bind(&sock).unwrap();
        let (tx, peer) = mpsc::channel();
        let done = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(HANG)).unwrap();
            stream.set_write_timeout(Some(HANG)).unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert!(request.starts_with("@s-a resources register start="));
            assert!(request.ends_with(" job=tree:program\n"));
            let pid = aterm_uds::fdpass::peer_pid(&stream).unwrap();
            let start = aterm_uds::process::read_process_start(i32::try_from(pid).unwrap())
                .unwrap()
                .boot
                .0;
            assert!(request.contains(&format!(" start={start} ")));
            tx.send(pid).unwrap();
            if refuse {
                writeln!(stream, "ERR denied").unwrap();
            } else {
                writeln!(
                    stream,
                    "OK resources version=1 root_pid={pid} start={start} session=s-a"
                )
                .unwrap();
            }
            request.clear();
            assert_eq!(
                reader.read_line(&mut request).unwrap(),
                0,
                "exec retained its control transport"
            );
        });
        Self {
            _dir: dir,
            sock,
            peer,
            done,
        }
    }

    fn command(&self, mode: &str) -> aterm_uds::spawnfd::Command {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(FIXTURE, format!("{}\n{mode}", self.sock.display()))
            .env("ATERM_PARENT_SESSION_ID", "s-a")
            .env("ATERM_SESSION_ID", "s-dead")
            .env_remove("ATERM_CONTROL_SOCK")
            .env_remove("ATERM_CTL_TOKEN");
        command
    }
}

#[test]
fn exec_keeps_the_registered_identity_exact_arguments_environment_and_status() {
    let host = Host::new(false);
    let output =
        aterm_uds::deadline::output_until(&mut host.command("exit"), Instant::now() + HANG)
            .unwrap();
    assert_eq!(output.status.code(), Some(37));
    let peer = host.peer.recv_timeout(HANG).unwrap();
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains(&format!("RUN pid={peer} stale=s-dead owner=s-a arg=<>"))
    );
    host.done.join().unwrap();
}

#[test]
fn a_refused_registration_never_executes_the_command() {
    let host = Host::new(true);
    let output =
        aterm_uds::deadline::output_until(&mut host.command("exit"), Instant::now() + HANG)
            .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("RUN pid=")
    );
    host.done.join().unwrap();
}

struct Owned(aterm_uds::spawnfd::Child);
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            signal_group(self.0.id(), 9);
            let _ = self.0.wait();
        }
    }
}

fn signal_group(pid: u32, signal: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: the process group is led by our unreaped child, never a shared
    // test-harness group or a recycled PID.
    assert_eq!(unsafe { kill(-i32::try_from(pid).unwrap(), signal) }, 0);
}

#[test]
fn group_stops_wait_for_the_actual_commands_delayed_exit() {
    for signal in [1, 2, 15] {
        let host = Host::new(false);
        let mut command = host.command("signal");
        command
            .process_group(0)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped());
        let mut child = Owned(command.spawn().unwrap());
        let stdout = child.0.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        loop {
            if lines.recv_timeout(HANG).unwrap().starts_with("READY pid=") {
                break;
            }
        }
        assert_eq!(host.peer.recv_timeout(HANG).unwrap(), child.0.id());
        signal_group(child.0.id(), signal);
        loop {
            if lines.recv_timeout(HANG).unwrap() == "SIGNAL" {
                break;
            }
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "admission released before command's signal handler finished"
        );
        writeln!(child.0.stdin.as_mut().unwrap(), "release").unwrap();
        child.0.stdin.take();
        let deadline = Instant::now() + HANG;
        let mut waiter = aterm_uds::deadline::ExitWait::new(&child.0);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "command exit hung");
            waiter.wait(deadline.saturating_duration_since(Instant::now()));
        };
        assert_eq!(status.code(), Some(37));
        host.done.join().unwrap();
        reader.join().unwrap();
    }
}

#[test]
fn exec_preserves_explicit_inherited_descriptors_above_stdio() {
    let host = Host::new(false);
    let first = host._dir.path().join("first");
    let second = host._dir.path().join("second");
    let mut command = aterm_uds::spawnfd::LaneCommand::new(std::env::current_exe().unwrap());
    command.args(["--exact", TEST, "--nocapture"]);
    command.env_exactly([
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("HOME".to_string(), host._dir.path().display().to_string()),
        ("ATERM_PARENT_SESSION_ID".to_string(), "s-a".to_string()),
        ("ATERM_SESSION_ID".to_string(), "s-dead".to_string()),
        (FIXTURE.to_string(), format!("{}\nfds", host.sock.display())),
    ]);
    let mut child = aterm_uds::spawnfd::spawn_with_two_fds(
        command,
        std::fs::File::create(&first).unwrap().into(),
        std::fs::File::create(&second).unwrap().into(),
    )
    .unwrap();
    let until = Instant::now() + HANG;
    let mut waiter = aterm_uds::deadline::ExitWait::on_pid(child.id());
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("descriptor control did not finish");
        }
        waiter.wait(until.saturating_duration_since(Instant::now()));
    };
    assert_eq!(status.code(), Some(37));
    assert_eq!(host.peer.recv_timeout(HANG).unwrap(), child.id());
    assert_eq!(
        std::fs::read_to_string(first).unwrap(),
        "first-descriptor\n"
    );
    assert_eq!(
        std::fs::read_to_string(second).unwrap(),
        "second-descriptor\n"
    );
    host.done.join().unwrap();
}
