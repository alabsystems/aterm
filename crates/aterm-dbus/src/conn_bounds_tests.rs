// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for authenticated connection deadline and queue bounds.

use super::*;
use std::net::Shutdown;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

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
        send_timeout: None,
    }
}

struct Peer {
    close: CtlStream,
    worker: Option<JoinHandle<std::io::Result<()>>>,
}

impl Drop for Peer {
    fn drop(&mut self) {
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

fn heard(bus: &mut CtlStream) -> Message {
    let mut buf = Vec::new();
    loop {
        if let Some(n) = message::frame_len(&buf).unwrap()
            && n <= buf.len()
        {
            return Message::decode(&buf[..n]).unwrap();
        }
        let mut chunk = [0u8; 4096];
        let n = bus.read(&mut chunk).unwrap();
        assert_ne!(n, 0, "the connection hung up");
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[test]
fn a_call_answered_after_its_deadline_leaves_the_connection_as_it_was() {
    let (ours, mut bus) = CtlStream::pair().unwrap();
    bus.set_read_timeout(Some(HANG)).unwrap();
    let mut conn = connected(ours);
    let ping = || Message::method_call(Some(":1.9"), "/private", None, "Ping").unwrap();
    let expired = Instant::now().checked_sub(Duration::from_secs(1));
    let outcome = conn.call_until(ping(), None, expired);
    assert!(matches!(outcome, Err(Error::NoReply)), "{outcome:?}");
    let sent = heard(&mut bus);
    assert_eq!(
        (sent.member.as_deref(), sent.serial),
        (Some("Ping"), 2),
        "the call went out whole"
    );
    bus.write_all(&bytes([answer(2), answer(3)])).unwrap();
    let late = conn.recv(Some(HANG)).unwrap().unwrap();
    assert_eq!(
        (late.kind, late.reply_serial),
        (Kind::MethodReturn, Some(2)),
        "the late answer is handed over as any other message"
    );
    let next = conn.call(ping(), HANG).unwrap();
    assert_eq!(
        next.reply_serial,
        Some(3),
        "and the connection carries the next call"
    );
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
        let result = conn.call(call, Duration::from_millis(1));
        let _ = done.send(matches!(result, Err(Error::Timeout)));
    });
    let outcome = received.recv_timeout(HANG);
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

#[test]
fn a_joins_handshake_ends_by_the_callers_bound_not_the_default() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("silent");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let addresses = address::parse(&format!("unix:path={}", path.display())).unwrap();
    let (done, joined) = mpsc::channel();
    let worker = thread::spawn(move || {
        // Frozen, the clock lapses no deadline: the join connects and sends its AUTH whatever
        // the load, and only the read timeout set from the handshake's deadline ends its wait.
        FROZEN_NOW.with(|frozen| frozen.set(Some(Instant::now())));
        let outcome = Connection::open_within(&addresses, Duration::from_millis(100));
        let _ = done.send(outcome.err());
    });
    let outcome = joined.recv_timeout(HANG);
    // The bus answers nothing, and accepts the join only now: its connection waits in the backlog.
    listener.set_nonblocking(true).unwrap();
    let (mut bus, _) = listener.accept().expect("the join connected");
    bus.set_nonblocking(false).unwrap();
    // A join still waiting hears the bus hang up and ends; one that gave up has hung up already.
    let _ = bus.shutdown(Shutdown::Write);
    worker.join().unwrap();
    let outcome = outcome.expect("the handshake outlived its caller's bound");
    assert!(matches!(outcome, Some(Error::Timeout)), "{outcome:?}");
    let mut heard = Vec::new();
    bus.read_to_end(&mut heard).unwrap();
    assert_eq!(
        heard,
        auth::first(effective_uid()),
        "the bound ended the handshake, with the join's AUTH sent and unanswered"
    );
}
