// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual private socket DATA controls; no fabricated native completion/owner.
use super::*;
use std::{io::Write as _, os::fd::AsFd as _};
fn endpoint() -> Instant {
    crate::fixture_deadline(Instant::now())
}
fn sockets() -> (Socket, Socket) {
    let (a, b) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    (Socket::new(a).unwrap(), Socket::new(b).unwrap())
}
#[test]
fn fragmented_header_and_payload_keep_exact_bounded_frame() {
    let (mut reader, mut writer) = sockets();
    for byte in [0, 0, 0, 3, b'a', b'b'] {
        (&writer.stream).write_all(&[byte]).unwrap();
        assert!(reader.receive(endpoint()).unwrap().is_none());
    }
    (&writer.stream).write_all(b"c").unwrap();
    assert_eq!(reader.receive(endpoint()).unwrap(), Some(b"abc".to_vec()));
    writer.push(b"next".to_vec()).unwrap();
    assert!(writer.flush(endpoint()).unwrap());
    assert_eq!(reader.receive(endpoint()).unwrap(), Some(b"next".to_vec()));
}
#[test]
fn oversize_or_zero_header_refuses_without_body_capture() {
    for size in [0, MAX_FRAME as u32 + 1, u32::MAX] {
        let (mut reader, writer) = sockets();
        (&writer.stream).write_all(&size.to_be_bytes()).unwrap();
        assert!(reader.receive(endpoint()).is_err());
        assert_eq!(reader.incoming.len(), 4);
        assert!(reader.length.is_none());
    }
}
#[test]
fn exact_maximum_and_frame_population_are_checked_before_queueing() {
    let (mut socket, _peer) = sockets();
    assert!(socket.push(vec![1; MAX_FRAME]).is_ok());
    assert!(socket.push(vec![1; MAX_FRAME + 1]).is_err());
    assert!(socket.push(Vec::new()).is_err());
    for _ in 1..40 {
        socket.push(vec![1]).unwrap();
    }
    assert!(socket.push(vec![1]).is_err());
    assert_eq!(socket.output.len(), 40);
}
fn delivery() -> (Delivery, Socket) {
    let (a, b) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    (
        Delivery::new(
            a,
            vec![b"prepared".to_vec(), b"terminal".to_vec()],
            b"exact bound ack".to_vec(),
        )
        .unwrap(),
        Socket::new(b).unwrap(),
    )
}
fn terminal(delivery: &mut Delivery, peer: &mut Socket) {
    assert!(!delivery.poll(endpoint()).unwrap());
    assert_eq!(
        peer.receive(endpoint()).unwrap(),
        Some(b"prepared".to_vec())
    );
    assert_eq!(
        peer.receive(endpoint()).unwrap(),
        Some(b"terminal".to_vec())
    );
    assert!(matches!(peer.peek().unwrap(), Peek::Eof));
}
#[test]
fn exact_ack_requires_original_terminal_eof_and_peer_eof() {
    let (mut delivery, mut peer) = delivery();
    terminal(&mut delivery, &mut peer);
    peer.push(b"exact bound ack".to_vec()).unwrap();
    peer.flush(endpoint()).unwrap();
    assert!(!delivery.poll(endpoint()).unwrap());
    peer.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(delivery.poll(endpoint()).unwrap());
}
#[test]
fn early_ack_and_peer_eof_are_not_completion() {
    let (mut delivery, mut peer) = delivery();
    peer.push(b"exact bound ack".to_vec()).unwrap();
    assert!(peer.flush(endpoint()).unwrap());
    assert!(delivery.poll(endpoint()).is_err());
    let (mut delivery, peer) = self::delivery();
    // Establish EOF even if a concurrent fork temporarily inherited the endpoint.
    // Dropping a socket alone can leave its nonblocking observer briefly waiting;
    // the observed EOF still cannot complete delivery.
    peer.shutdown(std::net::Shutdown::Write).unwrap();
    drop(peer);
    let deadline = endpoint();
    aterm_uds::ready::until(
        delivery.socket.stream.as_fd(),
        aterm_uds::ready::Interest::Read,
        deadline,
    )
    .unwrap();
    assert!(matches!(delivery.socket.peek().unwrap(), Peek::Eof));
    assert!(delivery.poll(deadline).is_err());
}
#[test]
fn wrong_or_trailed_ack_refuses_after_actual_terminal_eof() {
    for trailing in [false, true] {
        let (mut delivery, mut peer) = delivery();
        terminal(&mut delivery, &mut peer);
        peer.push(if trailing {
            b"exact bound ack".to_vec()
        } else {
            b"foreign bound ack".to_vec()
        })
        .unwrap();
        if trailing {
            peer.push(b"extra".to_vec()).unwrap();
        }
        peer.flush(endpoint()).unwrap();
        peer.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(delivery.poll(endpoint()).is_err());
    }
}
#[test]
fn original_expiry_refuses_before_socket_output() {
    let (mut delivery, peer) = delivery();
    assert!(delivery.poll(Instant::now()).is_err());
    assert!(matches!(peer.peek().unwrap(), Peek::Waiting));
}

#[test]
fn failed_partial_frame_drains_actual_eof_without_becoming_a_message() {
    let (mut reader, writer) = sockets();
    (&writer.stream).write_all(&[0, 0, 0, 9, 1, 2]).unwrap();
    assert!(reader.receive(endpoint()).unwrap().is_none());
    writer.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(reader.discard_to_eof().unwrap());
    assert_eq!(reader.received, 6);
    assert!(reader.incoming.is_empty());
}

#[test]
fn failed_control_never_reads_a_byte_beyond_original_intake() {
    let (mut reader, mut writer) = sockets();
    writer.push(vec![7; MAX_FRAME]).unwrap();
    let mut complete = None;
    let deadline = endpoint();
    // A turn is deliberately bounded; native socket capacity is not assumed.
    // At worst one byte progresses per turn, within this SAME finite endpoint.
    for _ in 0..=MAX_FRAME {
        writer.flush(deadline).unwrap();
        if let Some(frame) = reader.receive(deadline).unwrap() {
            complete = Some(frame);
            break;
        }
    }
    assert_eq!(complete.unwrap(), vec![7; MAX_FRAME]);
    assert_eq!(reader.received, MAX_FRAME + 4);
    (&writer.stream).write_all(&[1, 2]).unwrap();
    assert!(reader.discard_to_eof().is_err());
    assert_eq!(reader.received, MAX_FRAME + 4);
    let mut remaining = [0; 2];
    (&reader.stream).read_exact(&mut remaining).unwrap();
    assert_eq!(remaining, [1, 2]);
}
