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
