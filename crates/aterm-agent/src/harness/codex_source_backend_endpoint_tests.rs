// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Actual stream observations only; these tests establish no SDK readiness.
use super::*;
use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn observe(bytes: &[u8]) -> io::Result<SocketAddr> {
    let (mut reader, mut writer) = UnixStream::pair()?;
    let bytes = bytes.to_vec();
    let producer = std::thread::spawn(move || writer.write_all(&bytes));
    let descriptor = reader.as_fd().try_clone_to_owned()?;
    let result = Observation::default().read(
        &mut reader,
        descriptor.as_fd(),
        Instant::now() + Duration::from_secs(2),
        || true,
    );
    drop(reader);
    drop(descriptor);
    let _ = producer.join().expect("diagnostic producer did not panic");
    result
}

#[test]
fn retained_stream_handles_fragmented_colored_native_banner() {
    let (mut reader, mut writer) = UnixStream::pair().unwrap();
    let producer = std::thread::spawn(move || {
        for part in [
            b"startup diagnostic\n\x1b[1mcodex app-server (Web".as_slice(),
            b"Sockets)\x1b[0m\n  \x1b[2mlistening on:\x1b[0m ",
            b"\x1b[32mws://127.0.0.1:43210\x1b[0m\n  readyz: http://127.0.0.1:43210/readyz\n",
        ] {
            writer.write_all(part).unwrap();
        }
    });
    let descriptor = reader.as_fd().try_clone_to_owned().unwrap();
    let address = Observation::default()
        .read(
            &mut reader,
            descriptor.as_fd(),
            Instant::now() + Duration::from_secs(2),
            || true,
        )
        .unwrap();
    producer.join().unwrap();
    assert_eq!(address, "127.0.0.1:43210".parse().unwrap());
}

#[test]
fn untrusted_addresses_and_unscoped_lines_are_refused() {
    for bytes in [
        b"  listening on: ws://127.0.0.1:43210\n".as_slice(),
        b"codex app-server (WebSockets)\n  listening on: ws://0.0.0.0:43210\n",
        b"codex app-server (WebSockets)\n  listening on: ws://127.0.0.1:0\n",
        b"codex app-server (WebSockets)\n  listening on: ws://localhost:43210\n",
        b"codex app-server (WebSockets)\n  listening on: ws://127.0.0.1:43210/extra\n",
        b"codex app-server (WebSockets)\ncodex app-server (WebSockets)\n",
    ] {
        assert_eq!(
            observe(bytes).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    assert_eq!(
        observe(b"startup failed\n").unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn silent_stream_honors_deadline_and_current_owner() {
    let (mut reader, _writer) = UnixStream::pair().unwrap();
    let descriptor = reader.as_fd().try_clone_to_owned().unwrap();
    let mut state = Observation::default();
    assert_eq!(
        state
            .read(&mut reader, descriptor.as_fd(), Instant::now(), || true)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        state
            .read(
                &mut reader,
                descriptor.as_fd(),
                Instant::now() + Duration::from_secs(2),
                || false
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
fn excessive_line_is_bounded_before_accepting_an_endpoint() {
    let bytes = vec![b'x'; MAX_LINE + 1];
    assert_eq!(
        observe(&bytes).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        observe(&b"diagnostic\n".repeat(MAX_BYTES / 10 + 1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn invalid_output_cannot_leave_an_address_eligible_for_a_later_retry() {
    let mut state = Observation::default();
    assert_eq!(state.push(b"codex app-server (WebSockets)\n  listening on: ws://127.0.0.1:43210\n  listening on: ws://0.0.0.0:43210\n").unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert!(state.address().is_none());
    assert_eq!(
        state
            .push(b"codex app-server (WebSockets)\n  listening on: ws://127.0.0.1:43210\n")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(state.address().is_none());
}
