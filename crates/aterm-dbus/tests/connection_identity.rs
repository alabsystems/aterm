// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Only private socketpairs and an owned re-exec environment participate.
#![cfg(unix)]

use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStringExt as _;
use std::time::{Duration, Instant};

use aterm_dbus::{Connection, Error, Message, Value, address};
use aterm_uds::CtlStream;

const HANG: Duration = Duration::from_secs(60);
const CHILD: &str = "ATERM_DBUS_ADDRESS_TEST_CHILD";

fn line(stream: &mut CtlStream) -> Vec<u8> {
    let mut line = Vec::new();
    while !line.ends_with(b"\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        line.push(byte[0]);
        assert!(line.len() <= aterm_dbus::auth::MAX_LINE);
    }
    line
}

fn hello_name(name: &'static str) -> Result<String, Error> {
    let (ours, mut bus) = CtlStream::pair().unwrap();
    bus.set_read_timeout(Some(HANG)).unwrap();
    bus.set_write_timeout(Some(HANG)).unwrap();
    let served = std::thread::spawn(move || {
        assert!(line(&mut bus).starts_with(b"\0AUTH EXTERNAL "));
        bus.write_all(b"OK 8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5f\r\n")
            .unwrap();
        assert_eq!(line(&mut bus), b"BEGIN\r\n");
        let mut frame = vec![0; 16];
        bus.read_exact(&mut frame).unwrap();
        let size = aterm_dbus::message::frame_len(&frame).unwrap().unwrap();
        frame.resize(size, 0);
        bus.read_exact(&mut frame[16..]).unwrap();
        let hello = Message::decode(&frame).unwrap();
        assert_eq!(hello.member.as_deref(), Some("Hello"));
        let mut reply = Message::method_return(&hello).with_body(vec![Value::from(name)]);
        reply.serial = 1;
        bus.write_all(&reply.encode().unwrap()).unwrap();
    });
    let result = Connection::handshake(ours, 1000, None).map(|conn| conn.unique_name().to_owned());
    served.join().unwrap();
    result
}

#[test]
fn hello_cannot_assign_a_well_known_name_as_our_unique_identity() {
    let reply = hello_name("org.fake.Bus");
    assert!(matches!(reply, Err(Error::Malformed { .. })), "{reply:?}");
}

#[test]
fn hello_accepts_an_actual_unique_name() {
    assert_eq!(hello_name(":1.42").unwrap(), ":1.42");
}

fn address_child(test: &str, mode: &str) {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("dbus-address-")
        .tempdir()
        .unwrap();
    // The fallback is deliberately a regular sentinel, never a real bus. This
    // tests address selection only; no code here opens a connection to it.
    let bus = scratch.path().join("bus");
    std::fs::write(&bus, b"private non-socket sentinel").unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut command = aterm_uds::spawnfd::command(exe);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", scratch.path())
        .env("XDG_RUNTIME_DIR", scratch.path())
        .env(CHILD, mode)
        .args(["--exact", test, "--nocapture"]);
    if mode == "invalid" {
        command.env(
            "DBUS_SESSION_BUS_ADDRESS",
            std::ffi::OsString::from_vec(b"unix:path=/private-invalid-\xff".to_vec()),
        );
    }
    let output = aterm_uds::deadline::outputs_until(
        &mut [&mut command],
        Instant::now() + HANG,
        Duration::from_millis(250),
    )
    .pop()
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "owned {mode} child: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(bus).unwrap(), b"private non-socket sentinel");
}

#[test]
fn an_explicit_non_utf8_bus_address_is_an_error() {
    if std::env::var(CHILD).as_deref() == Ok("invalid") {
        let selected = address::session_bus();
        assert!(matches!(selected, Err(Error::Invalid(_))), "{selected:?}");
        return;
    }
    address_child("an_explicit_non_utf8_bus_address_is_an_error", "invalid");
}

#[test]
fn a_missing_address_can_select_the_private_runtime_fallback() {
    if std::env::var(CHILD).as_deref() == Ok("missing") {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap();
        assert_eq!(
            address::session_bus().unwrap(),
            vec![address::Address::unix_path(&format!("{runtime}/bus"))]
        );
        return;
    }
    address_child(
        "a_missing_address_can_select_the_private_runtime_fallback",
        "missing",
    );
}
