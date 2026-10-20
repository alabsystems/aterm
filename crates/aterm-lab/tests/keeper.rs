// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The windowed instance's keeper (`aterm-lab keeper -- PROGRAM`) ends what it
//! keeps however its launcher goes: a cut lifeline (a real FIFO, as
//! `Instance::start` hands it, whose end of file `poll(2)` on macOS never
//! reports) or a signal to the keeper itself. The kept program is a `sleep`
//! that prints its pid first, so a survivor is seen by pid.

#![cfg(target_os = "macos")]

use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use aterm_uds::lifeline::Lifeline;
use aterm_uds::spawnfd::Stdio;

/// A hang detector, never a latency budget: the keeper's own grace is 5 s.
const HANG: Duration = Duration::from_secs(60);

/// A keeper holding a `sleep`, its lifeline still held by this test.
struct Kept {
    keeper: i32,
    sleeper: i32,
    lifeline: Option<Lifeline>,
    exited: mpsc::Receiver<Option<i32>>,
    dir: PathBuf,
}

impl Kept {
    fn start(tag: &str) -> Kept {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("keeper-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let (lifeline, far) = Lifeline::open(&dir).expect("lifeline FIFO");
        let mut child = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_aterm-lab"))
            .args([
                "keeper",
                "--",
                "/bin/sh",
                "-c",
                "echo $$; exec /bin/sleep 600",
            ])
            .setsid(true)
            .stdin(Stdio::from(far))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start the keeper");
        let keeper = i32::try_from(child.id()).expect("pid");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("piped stdout"))
            .read_line(&mut line)
            .expect("the kept program's pid");
        let sleeper = line.trim().parse().expect("a pid line");
        let (tx, exited) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait().ok().and_then(|s| s.code()));
        });
        Kept {
            keeper,
            sleeper,
            lifeline: Some(lifeline),
            exited,
            dir,
        }
    }

    /// The keeper's exit status, once it has ended.
    fn exit_code(&self) -> Option<i32> {
        self.exited
            .recv_timeout(HANG)
            .expect("the keeper never ended (a hang detector, one minute)")
    }
}

impl Drop for Kept {
    fn drop(&mut self) {
        // A failed case leaves nothing running (SIGKILL by pid); a passing one
        // has seen both gone, so it signals nothing that could be a stranger.
        if std::thread::panicking() {
            if alive(self.sleeper) {
                signal(self.sleeper, libc::SIGKILL);
            }
            if self.exited.try_recv().is_err() {
                signal(self.keeper, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn signal(pid: i32, sig: i32) -> bool {
    // SAFETY: a plain `kill(2)` on a pid above 1 this test started.
    unsafe { libc::kill(pid, sig) == 0 }
}

/// Whether `pid` still names a process (`kill(pid, 0)`).
fn alive(pid: i32) -> bool {
    signal(pid, 0)
}

#[test]
fn a_cut_lifeline_ends_the_kept_program() {
    let mut k = Kept::start("cut");
    assert!(alive(k.sleeper), "the kept sleep runs");
    drop(k.lifeline.take());
    assert_eq!(k.exit_code(), Some(0), "a cut lifeline is a clean end");
    assert!(!alive(k.sleeper), "the kept sleep outlived its keeper");
}

#[test]
fn a_signalled_keeper_ends_the_kept_program() {
    let k = Kept::start("term");
    assert!(alive(k.sleeper), "the kept sleep runs");
    assert!(signal(k.keeper, libc::SIGTERM), "SIGTERM the keeper");
    assert_eq!(k.exit_code(), Some(128 + libc::SIGTERM));
    assert!(!alive(k.sleeper), "the kept sleep outlived its keeper");
}
