// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Connection`] against the session zbus 5.16.0 ran with a scripted bus
//! (tests/recorded/zbus-5.16.0-session.tsv): the replayed bus answers with the recorded bytes, and
//! everything aterm-dbus writes, from the SASL NUL to the last signal, must equal what zbus wrote.

#![cfg(unix)]

use std::io::{Read as _, Write as _};
use std::thread;
use std::time::Duration;

use crate::support::*;
use aterm_dbus::{Connection, Error, Message, Value};
use aterm_uds::CtlStream;

/// A wait for something that must happen: a hang detector, not a budget.
const HANG: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
struct Unit {
    dir: String,
    kind: String,
    bytes: Vec<u8>,
}

fn transcript(transport: &str) -> Vec<Unit> {
    let units: Vec<Unit> = rows("zbus-5.16.0-session.tsv")
        .into_iter()
        .filter(|r| r[0] == transport)
        .map(|r| Unit {
            dir: r[1].clone(),
            kind: r[2].clone(),
            bytes: unhex(r.get(3).map_or("", String::as_str)),
        })
        .collect();
    assert!(units.len() > 10, "{transport}: {} units", units.len());
    units
}

fn read_line(s: &mut CtlStream) -> std::io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        s.read_exact(&mut b)?;
        line.push(b[0]);
    }
    Ok(line)
}

fn read_message(s: &mut CtlStream) -> std::io::Result<Vec<u8>> {
    let mut all = vec![0u8; 16];
    s.read_exact(&mut all)?;
    let n = aterm_dbus::message::frame_len(&all)
        .ok()
        .flatten()
        .ok_or_else(|| std::io::Error::other("unframable message"))?;
    all.resize(n, 0);
    s.read_exact(&mut all[16..])?;
    Ok(all)
}

/// The replayed bus: writes each recorded `s2c` unit and reads one unit of the
/// recorded kind for each `c2s` one, returning what the client wrote.
fn replay(mut bus: CtlStream, script: Vec<Unit>) -> Vec<Unit> {
    bus.set_read_timeout(Some(HANG)).unwrap();
    let mut wrote = Vec::new();
    for unit in script {
        if unit.dir == "s2c" {
            bus.write_all(&unit.bytes).unwrap();
            continue;
        }
        let got = match unit.kind.as_str() {
            "line" => read_line(&mut bus),
            "msg" => read_message(&mut bus),
            _ => {
                let mut rest = Vec::new();
                bus.read_to_end(&mut rest).map(|_| rest)
            }
        };
        match got {
            Ok(bytes) => wrote.push(Unit {
                dir: unit.dir,
                kind: unit.kind,
                bytes,
            }),
            Err(e) => {
                eprintln!("replayed bus stopped: {e}");
                break;
            }
        }
    }
    wrote
}

/// What an AT-SPI publisher does first, the script zbus ran.
fn client(stream: CtlStream, uid: u32) {
    let mut conn = Connection::handshake(stream, uid, None).unwrap();
    assert_eq!(conn.unique_name(), ME);
    assert_eq!(conn.server_guid(), "8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5f");
    conn.add_match(RULE, HANG).unwrap();
    let get = |member: &str, iface: &str| {
        Message::method_call(Some("org.a11y.Bus"), "/org/a11y/bus", Some(iface), member).unwrap()
    };
    let address = conn.call(get("GetAddress", "org.a11y.Bus"), HANG).unwrap();
    assert_eq!(address.first_str(), Some(AT_SPI_ADDRESS));
    let refused = conn.call(
        get("Get", "org.freedesktop.DBus.Properties")
            .with_body(vec!["org.a11y.Status".into(), "ScreenReaderEnabled".into()]),
        HANG,
    );
    match refused {
        Err(Error::Remote { name, message }) => {
            assert_eq!(name, "org.freedesktop.DBus.Error.UnknownProperty");
            assert_eq!(message, "Unknown property 'ScreenReaderEnabled'");
        }
        other => panic!("Get answered {other:?}"),
    }
    conn.emit(
        "/org/a11y/atspi/accessible/7",
        "org.a11y.atspi.Event.Object",
        "StateChanged",
        event_body("focused", 1, 0, Value::U32(0)),
    )
    .unwrap();
    let queued = conn.recv(Some(HANG)).unwrap().expect("NameAcquired");
    assert_eq!(
        (queued.member.as_deref(), queued.first_str()),
        (Some("NameAcquired"), Some(ME))
    );
    assert!(!conn.has_buffered());
}

fn run(script: Vec<Unit>, uid: u32) -> Vec<Unit> {
    let (ours, bus) = CtlStream::pair().unwrap();
    let replayed = thread::spawn(move || replay(bus, script));
    client(ours, uid);
    replayed.join().unwrap()
}

fn written(script: &[Unit]) -> Vec<Unit> {
    script.iter().filter(|u| u.dir == "c2s").cloned().collect()
}

fn assert_same(ours: &[Unit], zbus: &[Unit]) {
    for (i, (a, b)) in ours.iter().zip(zbus).enumerate() {
        assert_eq!(
            (&a.kind, hex(&a.bytes)),
            (&b.kind, hex(&b.bytes)),
            "unit {i}"
        );
    }
    assert_eq!(ours.len(), zbus.len());
}

#[test]
fn the_client_writes_what_zbus_wrote_without_descriptor_passing() {
    let script = transcript("nofd");
    let ours = run(script.clone(), 1000);
    assert_same(&ours, &written(&script));
}

#[test]
fn over_a_unix_socket_zbus_differs_by_the_descriptor_negotiation_alone() {
    let script = transcript("unix");
    let negotiation =
        |u: &Unit| u.bytes == b"NEGOTIATE_UNIX_FD\r\n" || u.bytes == b"AGREE_UNIX_FD\r\n";
    assert_eq!(script.iter().filter(|u| negotiation(u)).count(), 2);
    let without: Vec<Unit> = script.iter().filter(|u| !negotiation(u)).cloned().collect();
    assert_eq!(
        without,
        transcript("nofd"),
        "the two recordings differ elsewhere too"
    );
    let ours = run(without.clone(), 1000);
    assert_same(&ours, &written(&without));
}

#[test]
fn another_uid_misses_the_recorded_first_line() {
    let script = transcript("nofd");
    let ours = run(script.clone(), 1001);
    assert_ne!(ours[0].bytes, written(&script)[0].bytes);
    assert_eq!(ours[0].bytes, b"\0AUTH EXTERNAL 31303031\r\n");
}
