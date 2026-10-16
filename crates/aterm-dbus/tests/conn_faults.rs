// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What [`Connection`] does with a bus that misbehaves after the handshake: a
//! well-framed message that breaks the specification or is larger than the
//! connection accepts is consumed and reported while the connection stays
//! usable; a framing fault ends it, and every later call says
//! [`Error::Closed`]; a server that refuses EXTERNAL fails the handshake.

#![cfg(unix)]

mod support;

use std::io::{Read as _, Write as _};
use std::thread;
use std::time::Duration;

use aterm_dbus::{Connection, Error, Message, Value};
use aterm_uds::CtlStream;
use support::*;

/// A wait for something that must happen: a hang detector, not a budget.
const HANG: Duration = Duration::from_secs(60);

fn recorded(name: &str) -> Vec<u8> {
    let row = rows("zbus-5.16.0-messages.tsv")
        .into_iter()
        .find(|r| r[0] == name)
        .unwrap();
    unhex(&row[1])
}

fn line(s: &mut CtlStream) -> Vec<u8> {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    while !out.ends_with(b"\r\n") {
        s.read_exact(&mut b).unwrap();
        out.push(b[0]);
    }
    out
}

/// A bus that accepts the handshake, answers Hello, then writes `after` and
/// either reads until the client closes or (`hang_up`) closes its end.
fn bus_then(after: Vec<u8>, hang_up: bool) -> (CtlStream, thread::JoinHandle<()>) {
    let (ours, mut bus) = CtlStream::pair().unwrap();
    let served = thread::spawn(move || {
        bus.set_read_timeout(Some(HANG)).unwrap();
        line(&mut bus);
        bus.write_all(b"OK 8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5f\r\n")
            .unwrap();
        assert_eq!(line(&mut bus), b"BEGIN\r\n");
        let mut hello = vec![0u8; recorded("hello").len()];
        bus.read_exact(&mut hello).unwrap();
        bus.write_all(&recorded("hello_reply")).unwrap();
        bus.write_all(&after).unwrap();
        if !hang_up {
            let mut rest = Vec::new();
            let _ = bus.read_to_end(&mut rest);
        }
    });
    (ours, served)
}

fn bus(after: Vec<u8>) -> (CtlStream, thread::JoinHandle<()>) {
    bus_then(after, false)
}

#[test]
fn a_bad_message_is_reported_and_the_next_one_still_arrives() {
    // name_acquired with its body's string length stomped: well framed, not
    // decodable. Then the intact signal.
    let mut bad = recorded("name_acquired");
    let body = bad.len() - 10;
    bad[body..body + 4].copy_from_slice(&200u32.to_le_bytes());
    let mut after = bad;
    after.extend_from_slice(&recorded("name_acquired"));
    let (ours, served) = bus(after);
    let mut conn = Connection::handshake(ours, 1000, None).unwrap();
    assert!(matches!(
        conn.recv(Some(HANG)),
        Err(Error::Malformed { .. })
    ));
    let next = conn.recv(Some(HANG)).unwrap().expect("the intact signal");
    assert_eq!(next.member.as_deref(), Some("NameAcquired"));
    assert!(
        conn.recv(Some(Duration::ZERO)).unwrap().is_none(),
        "nothing else is waiting"
    );
    drop(conn);
    served.join().unwrap();
}

#[test]
fn a_framing_fault_ends_the_connection() {
    let mut garbage = recorded("name_acquired");
    garbage[0] = b'x';
    let mut after = recorded("name_acquired");
    after.extend_from_slice(&garbage);
    let (ours, served) = bus(after);
    let mut conn = Connection::handshake(ours, 1000, None).unwrap();
    // A good message and bytes that frame none, in one write, so one read
    // takes both: a poll loop is told the fault waits, though the peer sends
    // nothing more.
    let first = conn.recv(Some(HANG)).unwrap().expect("the intact signal");
    assert_eq!(first.member.as_deref(), Some("NameAcquired"));
    assert!(conn.has_buffered(), "the fault is buffered");
    assert!(matches!(
        conn.recv(Some(HANG)),
        Err(Error::Malformed {
            what: "unknown byte order",
            ..
        })
    ));
    assert!(matches!(conn.recv(Some(HANG)), Err(Error::Closed)));
    assert!(
        conn.has_buffered(),
        "Closed is an answer a poll would not see"
    );
    let ping = Message::method_call(Some(":1.9"), "/", None, "Ping").unwrap();
    assert!(matches!(conn.send(ping), Err(Error::Closed)));
    drop(conn);
    served.join().unwrap();
}

#[test]
fn a_message_past_the_limit_is_read_past() {
    // Larger than one read (8 KiB) so its tail is dropped as it arrives, then
    // a message the connection takes.
    let mut big = Message::signal("/a", "org.a.B", "Big")
        .unwrap()
        .with_body(vec![Value::from("x".repeat(20_000).as_str())]);
    big.serial = 9;
    let mut after = big.encode().unwrap();
    after.extend_from_slice(&recorded("name_acquired"));
    let (ours, served) = bus(after);
    let mut conn = Connection::handshake(ours, 1000, None).unwrap();
    conn.set_max_incoming(1024);
    assert!(matches!(
        conn.recv(Some(HANG)),
        Err(Error::Malformed {
            what: "message larger than this connection accepts",
            ..
        })
    ));
    let next = conn.recv(Some(HANG)).unwrap().expect("the next message");
    assert_eq!(next.member.as_deref(), Some("NameAcquired"));
    // The control: at a limit that admits it, the same stream yields both.
    let (ours, served_too) = bus({
        let mut both = big.encode().unwrap();
        both.extend_from_slice(&recorded("name_acquired"));
        both
    });
    let mut wide = Connection::handshake(ours, 1000, None).unwrap();
    wide.set_max_incoming(64 * 1024);
    let first = wide.recv(Some(HANG)).unwrap().expect("the big signal");
    assert_eq!(first.member.as_deref(), Some("Big"));
    drop((conn, wide));
    served.join().unwrap();
    served_too.join().unwrap();
}

#[test]
fn a_hangup_is_closed_and_a_refusal_fails_the_handshake() {
    let (ours, served) = bus(Vec::new());
    let mut conn = Connection::handshake(ours, 1000, None).unwrap();
    assert!(conn.recv(Some(Duration::ZERO)).unwrap().is_none());
    drop(conn);
    served.join().unwrap();

    let (ours, mut theirs) = CtlStream::pair().unwrap();
    let refusing = thread::spawn(move || {
        theirs.set_read_timeout(Some(HANG)).unwrap();
        line(&mut theirs);
        theirs.write_all(b"REJECTED DBUS_COOKIE_SHA1\r\n").unwrap();
    });
    match Connection::handshake(ours, 1000, None) {
        Err(Error::Auth(why)) => assert!(why.contains("DBUS_COOKIE_SHA1"), "{why}"),
        other => panic!("handshake against a refusal: {:?}", other.err()),
    }
    refusing.join().unwrap();

    let (ours, mut theirs) = CtlStream::pair().unwrap();
    let wrong_guid = thread::spawn(move || {
        theirs.set_read_timeout(Some(HANG)).unwrap();
        line(&mut theirs);
        theirs
            .write_all(b"OK 8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5f\r\n")
            .unwrap();
    });
    let refused = Connection::handshake(ours, 1000, Some("00000000000000000000000000000000"));
    assert!(matches!(refused, Err(Error::Auth(_))));
    wrong_guid.join().unwrap();

    // A bus that writes and then hangs up: what it wrote still arrives (macOS
    // refuses a read timeout on the shut-down socket), then Closed.
    let (ours, served) = bus_then(recorded("name_acquired"), true);
    let mut conn = Connection::handshake(ours, 1000, None).unwrap();
    served.join().unwrap();
    let last = conn
        .recv(Some(HANG))
        .unwrap()
        .expect("the signal written before the hang-up");
    assert_eq!(last.member.as_deref(), Some("NameAcquired"));
    let closed = conn.recv(Some(HANG));
    assert!(matches!(closed, Err(Error::Closed)), "{closed:?}");
    assert!(matches!(conn.recv(Some(HANG)), Err(Error::Closed)));
}
