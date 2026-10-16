// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private peers exercise the already-authenticated connection state. No bus
//! daemon, session-bus address, or external service participates.

use super::*;
use std::net::Shutdown;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

/// A wait for something that must happen, never a latency assertion.
const HANG: Duration = Duration::from_secs(60);

fn connected(stream: CtlStream) -> Connection {
    Connection {
        stream,
        rbuf: Vec::new(),
        queue: VecDeque::new(),
        queued_bytes: 0,
        serial: 1,
        unique_name: ":1.42".into(),
        guid: "8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5f".into(),
        max_incoming: DEFAULT_MAX_INCOMING,
        discard: 0,
        broken: false,
    }
}

struct Peer {
    close: CtlStream,
    worker: Option<JoinHandle<std::io::Result<()>>>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        // Closing the owned peer also releases a writer blocked by the client
        // refusing a message. Join on ordinary completion and panic unwind.
        let _ = self.close.shutdown(Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn sending(bytes: Vec<u8>) -> (Connection, Peer) {
    let (ours, mut bus) = CtlStream::pair().unwrap();
    bus.set_write_timeout(Some(HANG)).unwrap();
    let close = bus.try_clone().unwrap();
    let worker = thread::spawn(move || bus.write_all(&bytes));
    (
        connected(ours),
        Peer {
            close,
            worker: Some(worker),
        },
    )
}

fn signal(serial: u32, body: &str) -> Message {
    let mut msg = Message::signal("/private", "org.aterm.Private", "Notice")
        .unwrap()
        .with_body(vec![Value::from(body)]);
    msg.serial = serial;
    msg
}

fn answer(serial: u32) -> Message {
    let mut call = Message::method_call(Some(":1.9"), "/private", None, "Ping").unwrap();
    call.serial = serial;
    let mut msg = Message::method_return(&call).with_body(vec![Value::from("answer")]);
    msg.serial = 1000;
    msg
}

fn bytes(messages: impl IntoIterator<Item = Message>) -> Vec<u8> {
    messages
        .into_iter()
        .flat_map(|msg| msg.encode().unwrap())
        .collect()
}

#[test]
fn an_expired_reply_deadline_preserves_the_unread_stream() {
    let (ours, mut bus) = CtlStream::pair().unwrap();
    // These two small frames fit the socket before reading starts. A worker
    // that merely intends to write is not proof that an expired read is ready.
    bus.write_all(&bytes([signal(2, "first"), answer(9)]))
        .unwrap();
    let mut conn = connected(ours);
    let expired = Instant::now().checked_sub(Duration::from_secs(1));
    let outcome = conn.wait_reply(9, expired);
    assert!(matches!(outcome, Err(Error::Timeout)), "{outcome:?}");
    let first = conn.recv(Some(HANG)).unwrap().unwrap();
    assert_eq!(first.first_str(), Some("first"));
    let reply = conn.recv(Some(HANG)).unwrap().unwrap();
    assert_eq!(reply.reply_serial, Some(9));
}

#[test]
fn a_reply_before_its_deadline_preserves_unrelated_messages_in_order() {
    let (mut conn, _peer) = sending(bytes([signal(2, "first"), signal(3, "second"), answer(9)]));
    let reply = conn
        .wait_reply(9, Instant::now().checked_add(HANG))
        .unwrap();
    assert_eq!(reply.first_str(), Some("answer"));
    for expected in ["first", "second"] {
        let queued = conn.recv(Some(Duration::ZERO)).unwrap().unwrap();
        assert_eq!(queued.first_str(), Some(expected));
    }
}

#[test]
fn unrelated_tiny_messages_cannot_grow_the_pending_queue_without_bound() {
    // More than the admitted count, while far below its byte allowance. A
    // matching answer at the end must not hide exhaustion along the way.
    let messages = (2..302)
        .map(|serial| signal(serial, "small"))
        .chain([answer(9)]);
    let (mut conn, _peer) = sending(bytes(messages));
    let outcome = conn.wait_reply(9, Instant::now().checked_add(HANG));
    assert!(matches!(outcome, Err(Error::Unsupported(_))), "{outcome:?}");
    assert!(conn.broken, "queue exhaustion closes the connection");
    let mut kept = 0;
    while let Ok(Some(msg)) = conn.recv(Some(Duration::ZERO)) {
        assert_eq!(msg.first_str(), Some("small"));
        kept += 1;
    }
    assert!(kept > 0 && kept < 300, "retained {kept} queued messages");
    assert!(matches!(
        conn.recv(Some(Duration::ZERO)),
        Err(Error::Closed)
    ));
}

#[test]
fn a_few_large_messages_cannot_evade_the_pending_byte_bound() {
    // Each legal message is below DEFAULT_MAX_INCOMING and the count is
    // small; their combined original wire bytes exceed one MiB.
    let body = "x".repeat(400_000);
    let (mut conn, _peer) = sending(bytes([
        signal(2, &body),
        signal(3, &body),
        signal(4, &body),
        answer(9),
    ]));
    let outcome = conn.wait_reply(9, Instant::now().checked_add(HANG));
    assert!(matches!(outcome, Err(Error::Unsupported(_))), "{outcome:?}");
    assert!(conn.broken, "byte exhaustion closes the connection");
}

#[test]
fn a_call_deadline_also_bounds_a_write_to_a_peer_that_never_reads() {
    let (ours, bus) = CtlStream::pair().unwrap();
    let cancel_write = ours.try_clone().unwrap();
    let (done, received) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut conn = connected(ours);
        let call = Message::method_call(Some(":1.9"), "/private", None, "Ping")
            .unwrap()
            .with_body(vec![Value::Str("x".repeat(8 << 20))]);
        // The condition under test is this API deadline, not machine speed.
        // The observer below allows a full minute for the result to arrive.
        let result = conn.call(call, Duration::from_millis(1));
        let _ = done.send(matches!(result, Err(Error::Timeout)));
    });
    let outcome = received.recv_timeout(HANG);
    // Always unblock and join the exact owned writer before asserting, also
    // on the baseline whose write does not respect the call deadline.
    let _ = cancel_write.shutdown(Shutdown::Both);
    let _ = bus.shutdown(Shutdown::Both);
    drop(cancel_write);
    drop(bus);
    worker.join().unwrap();
    assert!(
        outcome.is_ok(),
        "call did not return by its deadline: {outcome:?}"
    );
    assert!(outcome.unwrap(), "call did not report its timeout");
}
