// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::Read;

#[test]
fn challenge_requires_the_physical_socket_and_exact_sdk_binding() {
    let registry = Registry::default();
    let connection = registry.fixture_connection();
    let foreign = registry.fixture_connection();
    let (native, mut child) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    child
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .unwrap();
    let control = Control::new(native).unwrap();
    let member = "a".repeat(32);
    let operation = "b".repeat(32);
    assert_eq!(
        control.acknowledge(&connection, &member, &operation, &"0".repeat(32)),
        Err(Error::Denied)
    );
    control.begin(&connection, &member, &operation).unwrap();
    let mut bytes = [0; 40];
    child.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes[..8], b"ALABCTL1");
    let nonce = std::str::from_utf8(&bytes[8..]).unwrap().to_owned();
    assert!(hex(&nonce, 32));
    let mut wrong_nonce = nonce.as_bytes().to_vec();
    wrong_nonce[0] = if wrong_nonce[0] == b'0' { b'1' } else { b'0' };
    let wrong_nonce = String::from_utf8(wrong_nonce).unwrap();
    assert_eq!(
        control.acknowledge(&foreign, &member, &operation, &nonce),
        Err(Error::Denied)
    );
    assert_eq!(
        control.acknowledge(&connection, &"c".repeat(32), &operation, &nonce),
        Err(Error::Denied)
    );
    assert_eq!(
        control.acknowledge(&connection, &member, &"d".repeat(32), &nonce),
        Err(Error::Denied)
    );
    assert_eq!(
        control.acknowledge(&connection, &member, &operation, &wrong_nonce),
        Err(Error::Denied)
    );
    control.begin(&connection, &member, &operation).unwrap();
    child.set_nonblocking(true).unwrap();
    assert_eq!(
        child.read(&mut bytes).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    control
        .acknowledge(&connection, &member, &operation, &nonce)
        .unwrap();
    control
        .acknowledge(&connection, &member, &operation, &nonce)
        .unwrap();
    control.begin(&connection, &member, &operation).unwrap();
    assert_eq!(
        child.read(&mut bytes).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        control.begin(&foreign, &member, &operation),
        Err(Error::Stale)
    );
}

#[test]
fn disconnected_pending_challenge_is_stale_instead_of_waiting_forever() {
    let registry = Registry::default();
    let connection = registry.fixture_connection();
    let (native, _child) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    let control = Control::new(native).unwrap();
    assert_eq!(control.bound().err(), Some(Error::Busy));
    control
        .begin(&connection, &"a".repeat(32), &"b".repeat(32))
        .unwrap();
    assert_eq!(control.bound().err(), Some(Error::Busy));
    registry.connection_closed(connection.id);
    assert_eq!(control.bound().err(), Some(Error::Stale));
}

#[test]
fn attachment_rights_travel_only_after_the_original_physical_binding() {
    use std::io::Write;
    let registry = Registry::default();
    let connection = registry.fixture_connection();
    let (native, mut child) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    child
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .unwrap();
    let control = Control::new(native).unwrap();
    let (endpoint, mut frontend) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    frontend
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .unwrap();
    assert_eq!(control.send_attachment(&endpoint), Err(Error::Busy));
    let member = "e".repeat(32);
    let operation = "f".repeat(32);
    control.begin(&connection, &member, &operation).unwrap();
    let mut challenge = [0; 40];
    child.read_exact(&mut challenge).unwrap();
    assert_eq!(control.send_attachment(&endpoint), Err(Error::Busy));
    control
        .acknowledge(
            &connection,
            &member,
            &operation,
            std::str::from_utf8(&challenge[8..]).unwrap(),
        )
        .unwrap();
    let receiver = std::thread::spawn(move || {
        let mut marker = [0];
        let mut received = aterm_uds::fdpass::recv_with_fds(&child, &mut marker, 1).unwrap();
        assert_eq!(received.bytes, 1);
        assert_eq!(&marker, b"A");
        assert_eq!(received.fds.len(), 1);
        let stream = aterm_uds::CtlStream::from(received.fds.pop().unwrap());
        let mut nonce = [0; 32];
        child.read_exact(&mut nonce).unwrap();
        child.write_all(&nonce).unwrap();
        (stream, child)
    });
    control.send_attachment(&endpoint).unwrap();
    drop(endpoint);
    let (mut stream, _child) = receiver.join().unwrap();
    stream.write_all(b"original").unwrap();
    let mut bytes = [0; 8];
    frontend.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"original");
    registry.connection_closed(connection.id);
    assert_eq!(control.send_attachment(&stream), Err(Error::Stale));
}

#[test]
fn physical_attachment_receipt_survives_contended_control_metadata() {
    use std::io::Write;
    let registry = Registry::default();
    let connection = registry.fixture_connection();
    let (native, mut child) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    child
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .unwrap();
    let control = Control::new(native).unwrap();
    let member = "a".repeat(32);
    let operation = "b".repeat(32);
    control.begin(&connection, &member, &operation).unwrap();
    let mut challenge = [0; 40];
    child.read_exact(&mut challenge).unwrap();
    control
        .acknowledge(
            &connection,
            &member,
            &operation,
            std::str::from_utf8(&challenge[8..]).unwrap(),
        )
        .unwrap();
    let (endpoint, mut frontend) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    frontend
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .unwrap();
    std::thread::scope(|threads| {
        let sender = threads.spawn(|| control.send_attachment(&endpoint));
        let mut marker = [0];
        let mut received = aterm_uds::fdpass::recv_with_fds(&child, &mut marker, 1).unwrap();
        assert_eq!(marker, *b"A");
        assert_eq!(received.bytes, 1);
        assert_eq!(received.fds.len(), 1);
        let mut received = aterm_uds::CtlStream::from(received.fds.pop().unwrap());
        let mut nonce = [0; 32];
        child.read_exact(&mut nonce).unwrap();
        // Force contention exactly after native send and before its receipt.
        // This is physical custody only; admission remains Busy until unlocked.
        let metadata = control.state.lock().unwrap();
        child.write_all(&nonce).unwrap();
        assert_eq!(sender.join().unwrap(), Ok(()));
        assert_eq!(control.bound().err(), Some(Error::Busy));
        drop(metadata);
        control.bound().unwrap();
        received.write_all(b"kept").unwrap();
        let mut bytes = [0; 4];
        frontend.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"kept");
        child.set_nonblocking(true).unwrap();
        assert_eq!(
            child.read(&mut marker).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "the one delivery emitted no duplicate right or challenge"
        );
    });
}

#[test]
fn received_socket_pair_survives_senders_and_carrier_closure() {
    use std::io::Write;
    use std::os::fd::AsFd;
    // Kernel transport only: no constructor/member/Source/SDK admission.
    let (backend, frontend) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    let fence = aterm_uds::kept_out(|| frontend.try_clone()).unwrap();
    let (control, receiver) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    let (mut callback, mut client) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    assert_eq!(
        aterm_uds::fdpass::send_with_fds(&control, b"A", &[backend.as_fd()]).unwrap(),
        1
    );
    let nonce = b"0123456789abcdef0123456789abcdef";
    let header = [b"F1".as_slice(), nonce.as_slice()].concat();
    assert_eq!(
        aterm_uds::fdpass::send_with_fds(&callback, &header, &[frontend.as_fd()]).unwrap(),
        header.len()
    );
    let mut marker = [0];
    let mut back = aterm_uds::fdpass::recv_with_fds(&receiver, &mut marker, 1).unwrap();
    assert_eq!(back.bytes, 1);
    assert_eq!(back.fds.len(), 1);
    assert_eq!(marker, *b"A");
    let mut back = aterm_uds::CtlStream::from(back.fds.pop().unwrap());
    let mut received_header = [0; 34];
    let mut front = aterm_uds::fdpass::recv_with_fds(&client, &mut received_header, 1).unwrap();
    assert_eq!(front.fds.len(), 1);
    client
        .read_exact(&mut received_header[front.bytes..])
        .unwrap();
    assert_eq!(received_header, header.as_slice());
    let mut front = aterm_uds::CtlStream::from(front.fds.pop().unwrap());
    client.write_all(nonce).unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    let mut receipt = [0; 32];
    callback.read_exact(&mut receipt).unwrap();
    assert_eq!(&receipt, nonce);
    assert_eq!(callback.read(&mut marker).unwrap(), 0);
    // Match Control::send_attachment: sender custody ends only after actual
    // reception. A successful SCM_RIGHTS write alone is not a physical receipt.
    drop(backend);
    drop(frontend);
    drop(control);
    drop(receiver);
    drop(callback);
    drop(client);
    back.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    front
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    front.write_all(b"ping").unwrap();
    let mut bytes = [0; 4];
    back.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"ping");
    back.write_all(b"pong").unwrap();
    front.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"pong");
    fence.shutdown(std::net::Shutdown::Both).unwrap();
    assert_eq!(back.read(&mut bytes).unwrap(), 0);
    assert_eq!(
        front.write(b"after fence").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
}

#[test]
fn wrong_physical_receipt_poisoning_cannot_be_replayed_as_another_delivery() {
    use std::io::Write;
    // Protocol principal fixture only: no native Source or SDK Ready grant.
    let registry = Registry::default();
    let connection = registry.fixture_connection();
    let (native, mut child) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    let control = Control::new(native).unwrap();
    let member = "a".repeat(32);
    let operation = "b".repeat(32);
    control.begin(&connection, &member, &operation).unwrap();
    let mut challenge = [0; 40];
    child.read_exact(&mut challenge).unwrap();
    control
        .acknowledge(
            &connection,
            &member,
            &operation,
            std::str::from_utf8(&challenge[8..]).unwrap(),
        )
        .unwrap();
    let (endpoint, _frontend) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    let receiver = std::thread::spawn(move || {
        let mut marker = [0];
        let _received = aterm_uds::fdpass::recv_with_fds(&child, &mut marker, 1).unwrap();
        let mut nonce = [0; 32];
        child.read_exact(&mut nonce).unwrap();
        nonce[0] = if nonce[0] == b'0' { b'1' } else { b'0' };
        child.write_all(&nonce).unwrap();
    });
    assert_eq!(control.send_attachment(&endpoint), Err(Error::Denied));
    receiver.join().unwrap();
    assert_eq!(control.bound().err(), Some(Error::Stale));
    assert_eq!(control.send_attachment(&endpoint), Err(Error::Stale));
}
