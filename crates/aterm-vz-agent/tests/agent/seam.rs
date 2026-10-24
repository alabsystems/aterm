// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bin itself on the macOS seam: it dials the watch socket, says `Hello` with the
//! SHA-256 of its own executable, is `Ready`, and serves a command.

use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;

use aterm_vz_agent::wire::{Exit, Frame, PROTO};

use super::{Conn, HANG, agent_exe};

/// The seam bin, killed by its pid however the test ends: a failed assertion must not
/// leave it serving, holding the test's output open.
struct Running(aterm_uds::spawnfd::Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_seam_bin_says_hello_with_its_own_sha256_and_serves_a_command() {
    let exe = agent_exe();
    let dir = aterm_tempfile::tempdir().unwrap();
    let (listen, watch) = (dir.path().join("c.sock"), dir.path().join("w.sock"));
    let watching = UnixListener::bind(&watch).unwrap();
    let child = aterm_uds::spawnfd::command(&exe)
        .arg("--listen")
        .arg(&listen)
        .arg("--watch")
        .arg(&watch)
        .args(["--tick-ms", "20"])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .unwrap();
    let _running = Running(child);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(watching.accept().map(|(s, _)| s));
    });
    let accepted = rx
        .recv_timeout(HANG)
        .expect("hung: the bin never dialed the watch socket");
    let mut host = Conn::new(accepted.unwrap());

    let Some(Frame::Hello(hello)) = host.frame() else {
        panic!("the first frame is Hello");
    };
    assert_eq!(hello.proto, PROTO);
    assert_eq!(
        hello.agent_sha256,
        aterm_vz_agent::sha256_of(&exe).unwrap(),
        "the hello's hash is the binary's"
    );
    let ready = loop {
        match host.frame() {
            Some(Frame::Booting(_)) => {}
            Some(Frame::Ready(r)) => break r,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(ready.state, "running");

    // `Ready` implies the command socket is bound: the bin binds it before it dials.
    let mut c = Conn::new(UnixStream::connect(&listen).unwrap());
    let exec = aterm_vz_agent::wire::Exec {
        argv: vec![
            exe.as_os_str().as_encoded_bytes().to_vec(),
            "stand-in".into(),
            "exit".into(),
            "7".into(),
        ],
        cwd: b"/".to_vec(),
        env: Vec::new(),
        stdin: aterm_vz_agent::wire::Stdin::Null,
        merge: false,
        clock_unix_ns: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64,
    };
    c.send(&Frame::Exec(exec));
    assert_eq!(c.ended().exit, Exit::Exited(7));
    // The seam changes nothing of this Mac's: no mount, no power-off.
    let mut m = Conn::new(UnixStream::connect(&listen).unwrap());
    m.send(&Frame::Poweroff);
    assert!(matches!(m.ended().exit, Exit::NotStarted(_)));
}
