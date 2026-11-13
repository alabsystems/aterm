// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Live as Port, System};
use std::io::{Read, Write};

/// A connection to the relay with a one-minute hang detector on its reads.
fn connect(port: u16) -> TcpStream {
    let s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    s
}

fn read_exactly(s: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0; n];
    s.read_exact(&mut buf).unwrap();
    buf
}

#[test]
fn each_connection_is_its_own_run_and_close_ends_every_run() {
    let port = Port::new();
    let cat = Cmd::new("/bin/cat");
    let mut relay = listen(&cat, port.child_env(&cat)).unwrap();
    let mut one = connect(relay.port());
    let mut two = connect(relay.port());
    one.write_all(b"one\x00\xff").unwrap();
    two.write_all(b"two").unwrap();
    assert_eq!(read_exactly(&mut one, 5), b"one\x00\xff");
    assert_eq!(read_exactly(&mut two, 3), b"two");
    relay.close().unwrap();
    for s in [&mut one, &mut two] {
        let mut rest = Vec::new();
        assert_eq!(s.read_to_end(&mut rest).unwrap(), 0);
    }
    assert!(
        relay.listener.upgrade().is_none(),
        "nothing listens after the close"
    );
    relay.close().unwrap();
}

#[test]
fn a_run_that_ends_closes_its_connection_because_the_relay_keeps_no_copy() {
    let port = Port::new();
    let mut relay = port
        .relay(&Cmd::new("/bin/echo").arg("RFB 003.889"))
        .unwrap();
    let mut s = connect(relay.port());
    let mut got = Vec::new();
    s.read_to_end(&mut got).unwrap();
    assert_eq!(got, b"RFB 003.889\n");
    relay.close().unwrap();
}

#[test]
fn a_run_that_does_not_start_is_reported_at_the_close() {
    let port = Port::new();
    let mut relay = port.relay(&Cmd::new("/nonexistent/ssh")).unwrap();
    let mut s = connect(relay.port());
    let mut got = Vec::new();
    s.read_to_end(&mut got).unwrap();
    assert!(got.is_empty());
    let why = relay.close().unwrap_err();
    assert!(why.starts_with("relay: /nonexistent/ssh: "), "{why}");
}

#[test]
fn a_run_a_connection_cannot_carry_is_refused_before_anything_listens() {
    let port = Port::new();
    let user = crate::model::User {
        name: "admin".into(),
        uid: 501,
        gid: 20,
        home: "/Users//admin".into(),
        shell: "/bin/zsh".into(),
        admin: true,
    };
    for cmd in [
        Cmd::new("cat"),
        Cmd::new("/bin/cat").as_user(&user),
        Cmd::new("/bin/cat").stdin_file("/dev/null"),
        Cmd::new("/bin/cat").until_deadline(),
    ] {
        let refused = port.relay(&cmd).err();
        assert!(
            matches!(refused, Some(RunFailed::Refused(_))),
            "{cmd:?}: {refused:?}"
        );
    }
}
