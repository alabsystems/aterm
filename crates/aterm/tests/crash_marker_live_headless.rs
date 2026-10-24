// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Crash marker lifecycle tests across real headless aterm processes sharing one log directory.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::{headless_boot, launch_isolation};

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

struct World(PathBuf);

impl Drop for World {
    fn drop(&mut self) {
        headless_boot::remove_world(&self.0);
    }
}

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

fn world() -> World {
    let name = format!("atcm-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let root = base.join(&name);
        if !launch_isolation::sock_fits(&root.join("run/aterm/c.sock.token")) {
            continue;
        }
        if launch_isolation::prepare(&root).is_ok() {
            return World(root);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    aterm_spec::host::could_not_run("no scratch base with a socket path short enough for sun_path")
}

fn logs_dir(root: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        root.join("home/Library/Logs/aterm")
    } else {
        root.join("state/aterm/logs")
    }
}

fn boot(root: &Path, tag: &str) -> Instance {
    let sock = root.join(format!("run/aterm/{tag}.sock"));
    let log_path = root.join(format!("{tag}.log"));
    let (out, err) = match std::fs::File::create(&log_path).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot open the instance log ({e})")),
    };
    let mut cmd = Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .arg("--control-sock")
        .arg(&sock)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let lifeline = launch_isolation::lifeline(&mut cmd, root);
    let mut ready = headless_boot::arm_ready(&mut cmd, root);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot launch aterm --headless ({e})")),
    };
    let mut instance = Instance {
        child,
        _lifeline: lifeline,
        sock,
    };
    headless_boot::await_ready(
        &mut instance,
        |i| &mut i.child,
        &mut ready,
        &log_path,
        |_| true,
    );
    instance
}

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

fn request(sock: &Path, line: &str) -> String {
    let token = std::fs::read_to_string(format!("{}.token", sock.display())).unwrap();
    let mut stream = UnixStream::connect(sock).unwrap();
    stream.set_read_timeout(Some(HANG)).unwrap();
    write!(stream, "AUTH {}\n{line}\n", token.trim()).unwrap();
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).unwrap();
    reply.trim_end().to_string()
}

fn first_sid(sock: &Path) -> String {
    let token = std::fs::read_to_string(format!("{}.token", sock.display())).unwrap();
    let mut stream = UnixStream::connect(sock).unwrap();
    stream.set_read_timeout(Some(HANG)).unwrap();
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

fn wait_until(what: &str, mut pred: impl FnMut() -> bool) {
    let seen = aterm_spec::wait::within(HANG, Duration::from_millis(25), || pred().then_some(()));
    assert!(seen.is_some(), "timed out waiting for {what}");
}

#[test]
fn another_start_never_unlinks_a_live_marker_and_a_clean_exit_removes_its_own() {
    let world = world();
    let root = world.0.clone();

    let mut a = boot(&root, "a");
    let a_pid = a.child.id();
    let a_marker = markers_of(&root, a_pid);
    assert_eq!(a_marker.len(), 1, "A armed one marker: {a_marker:?}");
    assert!(
        a_marker[0].ends_with("-other.log"),
        "headless is never `app`"
    );

    let mut b = boot(&root, "b");
    let b_pid = b.child.id();
    assert_eq!(markers_of(&root, b_pid).len(), 1);
    assert_eq!(
        markers_of(&root, a_pid),
        a_marker,
        "B's sweep kept A's marker"
    );

    b.child.kill().unwrap();
    b.child.wait().unwrap();
    assert_eq!(
        markers_of(&root, b_pid).len(),
        1,
        "a SIGKILL runs no exit path"
    );
    let _c = boot(&root, "c");
    assert!(
        markers_of(&root, b_pid).is_empty(),
        "C's sweep removed dead, not-news B's corpse"
    );
    assert_eq!(
        markers_of(&root, a_pid),
        a_marker,
        "C's sweep kept A's marker"
    );

    let sid = first_sid(&a.sock);
    let closed = request(&a.sock, &format!("@{sid} close"));
    assert!(closed.starts_with("OK closed"), "{closed}");
    let status = a.child.wait().unwrap();
    assert!(status.success(), "A quit cleanly: {status:?}");
    wait_until("A's marker to go", || markers_of(&root, a_pid).is_empty());
}
