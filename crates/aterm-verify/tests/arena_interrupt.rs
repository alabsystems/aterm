// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Interrupted arena run cleanup tests.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::mpsc;
use std::time::Duration;

use aterm_uds::process::pid_alive;
use aterm_uds::spawnfd::{Stdio, command};

/// A wait for something that must happen.
const HANG: Duration = Duration::from_secs(60);

/// SIGKILLs whatever is still listed when a failed assertion unwinds.
struct Reaper(Vec<u32>);

impl Drop for Reaper {
    fn drop(&mut self) {
        for pid in &self.0 {
            let _ = command("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .stderr(Stdio::null())
                .status();
        }
    }
}

enum Event {
    /// What the stand-in reported through the FIFO.
    Ready(std::io::Result<String>),
    Exited(std::io::Result<ExitStatus>),
}

/// The stand-in: `--version` names aterm (the lane refuses anything else).
fn stand_in(dir: &Path, fifo: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let made = command("mkfifo").arg(fifo).status().expect("mkfifo");
    assert!(made.success(), "mkfifo {}", fifo.display());
    let stub = dir.join("aterm");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\n\
             case \"$1\" in --version) echo 'aterm 0.0.0 (interrupt stand-in)'; exit 0;; esac\n\
             printf '%s\\n%s\\n%s\\n' \"$$\" \"$HOME\" \"$3\" > '{}'\n\
             exec /bin/sleep 600\n",
            fifo.display()
        ),
    )
    .expect("write the stand-in");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    stub
}

/// `cmd` with `signo` at its default disposition in the child alone.
fn with_default(cmd: &mut std::process::Command, signo: i32) -> &mut std::process::Command {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: the hook runs between fork and exec and calls only `sigaction`,
    // which is async-signal-safe; the struct it reads is on the child's stack.
    unsafe {
        cmd.pre_exec(move || {
            let mut dfl: libc::sigaction = std::mem::zeroed();
            dfl.sa_sigaction = libc::SIG_DFL;
            if libc::sigaction(signo, &raw const dfl, std::ptr::null_mut()) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        })
    }
}

fn interrupted_by(signal: &str, signo: i32) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-interrupt-")
        .tempdir()
        .expect("scratch");
    let fifo = dir.path().join("ready");
    let stub = stand_in(dir.path(), &fifo);
    let log = dir.path().join("runner.log");
    let mut runner = with_default(
        &mut std::process::Command::new(env!("CARGO_BIN_EXE_aterm-verify")),
        signo,
    )
    .args([
        "perf",
        "start",
        "--samples",
        "1",
        "--timeout-s",
        "600",
        // The stand-in times nothing: start the lane at once, not on a quiet machine.
        "--admit",
        "now",
        "--binary",
    ])
    .arg(&stub)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::fs::File::create(&log).expect("log"))
    .spawn()
    .expect("spawn aterm-verify");
    let runner_pid = runner.id();
    let mut reaper = Reaper(vec![runner_pid]);
    let why = || std::fs::read_to_string(&log).unwrap_or_default();

    let (tx, rx) = mpsc::channel();
    let exited = tx.clone();
    std::thread::spawn(move || {
        let _ = exited.send(Event::Exited(runner.wait()));
    });
    let reader = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(Event::Ready(std::fs::read_to_string(&reader)));
    });

    let report = match rx.recv_timeout(HANG) {
        Ok(Event::Ready(Ok(text))) => text,
        Ok(Event::Ready(Err(e))) => panic!("reading the stand-in's report: {e}"),
        Ok(Event::Exited(status)) => panic!(
            "aterm-verify exited ({status:?}) before launching the stand-in:\n{}",
            why()
        ),
        Err(_) => panic!("no launch within {HANG:?}:\n{}", why()),
    };
    let mut lines = report.lines();
    let stand_in_pid: u32 = lines
        .next()
        .and_then(|l| l.parse().ok())
        .expect("the stand-in's pid");
    reaper.0.push(stand_in_pid);
    let home = PathBuf::from(lines.next().expect("its HOME"));
    let sock = PathBuf::from(lines.next().expect("its --control-sock"));
    let run_root = home
        .parent()
        .and_then(Path::parent)
        .expect("HOME sits in <run>/sample-1/home")
        .to_path_buf();
    let sock_dir = sock.parent().expect("socket dir").to_path_buf();
    assert!(pid_alive(stand_in_pid));
    assert!(run_root.is_dir() && sock_dir.is_dir());

    let sent = command("/bin/kill")
        .arg(signal)
        .arg(runner_pid.to_string())
        .status()
        .expect("kill");
    assert!(sent.success());
    match rx.recv_timeout(HANG) {
        Ok(Event::Exited(Ok(status))) => assert_eq!(
            status.code(),
            Some(128 + signo),
            "{signal}: {status}\n{}",
            why()
        ),
        Ok(Event::Exited(Err(e))) => panic!("waiting for aterm-verify: {e}"),
        Ok(Event::Ready(_)) => panic!("a second report"),
        Err(_) => panic!("aterm-verify outlived {signal} by {HANG:?}:\n{}", why()),
    }
    assert!(
        !pid_alive(stand_in_pid),
        "{signal}: the stand-in {stand_in_pid} outlived the run"
    );
    assert!(
        !run_root.exists(),
        "{signal}: {} was left behind",
        run_root.display()
    );
    assert!(
        !sock_dir.exists(),
        "{signal}: {} was left behind",
        sock_dir.display()
    );
}

#[test]
fn an_interrupted_arena_run_retires_its_terminal_and_its_scratch() {
    for (signal, signo) in [("-INT", 2), ("-TERM", 15), ("-HUP", 1)] {
        interrupted_by(signal, signo);
    }
}
