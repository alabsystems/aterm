// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::{AcceptWait, AcceptedMode};
#[test]
fn renewing_a_command_cannot_extend_the_overall_mail_deadline() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) =
        aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
    let mut wire = Wire::for_test(client);
    wire.overall = Some(Instant::now());
    wire.renew(Duration::from_secs(60));
    assert!(wire.deadline.remaining().is_none());
    assert!(wire.write(b"a1 NOOP\r\n").is_err());
    server.set_nonblocking(true).unwrap();
    assert_eq!(
        server.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn bounded_lines_and_arguments_refuse_injection() {
    assert!(bounded_line(&mut &b"* OK\n"[..]).is_err());
    assert!(bounded_line(&mut &vec![b'x'; MAX_LINE + 1][..]).is_err());
    assert!(quoted("INBOX\r\nLOGOUT").is_err());
    assert_eq!(quoted("a\\b\"c").unwrap(), "\"a\\\\b\\\"c\"");
}
