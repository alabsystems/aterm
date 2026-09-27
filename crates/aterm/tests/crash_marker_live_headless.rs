// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crash marker's lifecycle across REAL aterm processes sharing one log
//! dir (`aterm-gui`'s `crash_signal::markers`).
//!
//! THE DEFECT (measured 2026-09-24 on the owner's Mac, 0.93.0): every GUI-mode
//! start swept every empty crash marker in the log dir, owner alive or not. A
//! stray start (pid 87172) unlinked the running daily driver's marker 20 ms
//! after arming its own, so a SIGSEGV of the daily driver would have left no
//! artifact; and because a clean run never removed its own marker, a SIGKILL
//! looked exactly like a clean quit. What this pins, one scratch `$HOME`, three
//! headless instances:
//!
//! 1. B starting does NOT remove A's marker (A is alive, holding its lock);
//! 2. B SIGKILLed leaves its marker; C starting removes it (B is gone and B was
//!    not the installed app, so its corpse is not news) — and still keeps A's;
//! 3. A closing its last tab (a clean `exit(3)`) removes A's own marker.
//!
//! Headless starts arm as `other`: only the installed app's `app` markers are
//! ever reported as "killed" (unit-tested in `crash_signal::marker_tests` and
//! `logging::tests`), which is the scoping that keeps harness SIGKILLs like
//! step 2 from being reported to the owner.
//!
//! ISOLATION: scratch HOME/XDG roots, private control sockets, a config with
//! every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`). A headless instance never reaches
//! WindowServer. SKIP (not fail) when an instance cannot boot.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

const MAX_SOCK_PATH: usize = 100;
const BOOT_DEADLINE: Duration = Duration::from_secs(60);

/// One scratch world every instance of the test shares; removed on drop.
struct World(PathBuf);

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One headless instance, killed on drop.
struct Instance {
    child: Child,
    /// Cut after `child` is killed (fields drop after `Drop::drop`), and closed by
    /// the kernel if this test process dies first: the instance goes with it.
    _lifeline: aterm_uds::lifeline::Lifeline,
    sock: PathBuf,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn world() -> Option<World> {
    let name = format!("atcm-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let root = base.join(&name);
        if root.join("run/aterm/c.sock.token").as_os_str().len() >= MAX_SOCK_PATH {
            continue;
        }
        if launch_isolation::prepare(&root).is_ok() {
            return Some(World(root));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    None
}

/// The log dir `aterm_types::dirs::logs_dir` resolves under the scratch world.
fn logs_dir(root: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        root.join("home/Library/Logs/aterm")
    } else {
        root.join("state/aterm/logs")
    }
}

/// Boot one headless instance on its own socket in `root`; `None` = SKIP.
fn boot(root: &Path, tag: &str) -> Option<Instance> {
    let sock = root.join(format!("run/aterm/{tag}.sock"));
    let log = std::fs::File::create(root.join(format!("{tag}.log"))).ok()?;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .arg("--control-sock")
        .arg(&sock)
        .stdin(Stdio::null())
        .stdout(log.try_clone().ok()?)
        .stderr(log);
    let lifeline = launch_isolation::lifeline(&mut cmd, root);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("SKIP: cannot launch aterm --headless ({e})");
            return None;
        }
    };
    let instance = Instance {
        child,
        _lifeline: lifeline,
        sock,
    };
    let deadline = Instant::now() + BOOT_DEADLINE;
    while !launch_isolation::control_listening(&instance.sock) {
        if Instant::now() > deadline {
            eprintln!("SKIP: instance {tag} never listened on its control socket");
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Some(instance)
}

/// The markers in the log dir armed by `pid`.
fn markers_of(root: &Path, pid: u32) -> Vec<String> {
    let prefix = format!("crash-marker-{pid}-");
    std::fs::read_dir(logs_dir(root))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(&prefix) && n.ends_with(".log"))
                .collect()
        })
        .unwrap_or_default()
}

/// One owner-token request on `sock`; the status line back.
fn request(sock: &Path, line: &str) -> String {
    let token = std::fs::read_to_string(format!("{}.token", sock.display())).unwrap();
    let mut stream = UnixStream::connect(sock).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(stream, "AUTH {}\n{line}\n", token.trim()).unwrap();
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).unwrap();
    reply.trim_end().to_string()
}

/// The instance's first session id, from `sessions` (`<local> <sid> …`).
fn first_sid(sock: &Path) -> String {
    let token = std::fs::read_to_string(format!("{}.token", sock.display())).unwrap();
    let mut stream = UnixStream::connect(sock).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(stream, "AUTH {}\nsessions\n", token.trim()).unwrap();
    let mut reader = BufReader::new(&stream);
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    let mut row = String::new();
    reader.read_line(&mut row).unwrap();
    row.split_whitespace()
        .find(|t| t.starts_with("s-"))
        .unwrap_or_else(|| panic!("no sid in `{status}` / `{row}`"))
        .to_string()
}

/// Wait until `pred` holds (a bounded poll on an EVENT, not a sleep verdict).
fn wait_until(what: &str, mut pred: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !pred() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn another_start_never_unlinks_a_live_marker_and_a_clean_exit_removes_its_own() {
    let Some(world) = world() else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let root = world.0.clone();

    let Some(mut a) = boot(&root, "a") else {
        return;
    };
    let a_pid = a.child.id();
    let a_marker = markers_of(&root, a_pid);
    assert_eq!(a_marker.len(), 1, "A armed one marker: {a_marker:?}");
    assert!(
        a_marker[0].ends_with("-other.log"),
        "headless is never `app`"
    );

    // 1. B's start sweeps the dir: A's live marker survives it.
    let Some(mut b) = boot(&root, "b") else {
        return;
    };
    let b_pid = b.child.id();
    assert_eq!(markers_of(&root, b_pid).len(), 1);
    assert_eq!(
        markers_of(&root, a_pid),
        a_marker,
        "B's sweep kept A's marker"
    );

    // 2. B dies without an exit path; its marker stays until C's start.
    b.child.kill().unwrap();
    b.child.wait().unwrap();
    assert_eq!(
        markers_of(&root, b_pid).len(),
        1,
        "a SIGKILL runs no exit path"
    );
    let Some(_c) = boot(&root, "c") else { return };
    assert!(
        markers_of(&root, b_pid).is_empty(),
        "C's sweep removed dead, not-news B's corpse"
    );
    assert_eq!(
        markers_of(&root, a_pid),
        a_marker,
        "C's sweep kept A's marker"
    );

    // 3. A closes its last tab: a clean exit removes A's own marker.
    let sid = first_sid(&a.sock);
    let closed = request(&a.sock, &format!("@{sid} close"));
    assert!(closed.starts_with("OK closed"), "{closed}");
    let status = a.child.wait().unwrap();
    assert!(status.success(), "A quit cleanly: {status:?}");
    wait_until("A's marker to go", || markers_of(&root, a_pid).is_empty());
}
