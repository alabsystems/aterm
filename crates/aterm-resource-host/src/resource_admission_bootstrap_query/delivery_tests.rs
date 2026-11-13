// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::{io::Write as _, time::Duration};

#[test]
fn actual_socket_requires_eof_before_one_exact_ack_and_keeps_accepted_owner() {
    let (mut client, server) = CtlStream::pair().unwrap();
    let mut delivery = Delivery::frames(server, vec![b"native buffered output".to_vec()]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    assert!(!delivery.poll(deadline).unwrap());
    let mut actual = Vec::new();
    client.read_to_end(&mut actual).unwrap();
    assert_eq!(&actual[..4], &(22u32.to_be_bytes()));
    assert_eq!(&actual[4..], b"native buffered output");
    client.write_all(&[0, 0, 0, 1, 1]).unwrap();
    assert!(!delivery.poll(deadline).unwrap());
    assert!(!delivery.control_closed(deadline).unwrap());
    client.write_all(&[1]).unwrap();
    assert!(delivery.poll(deadline).is_err());
    assert!(delivery.poll(deadline).is_err());
}

#[test]
fn early_ack_and_peer_loss_never_become_a_positive_delivery() {
    for close in [false, true] {
        let (mut client, server) = CtlStream::pair().unwrap();
        if !close {
            client.write_all(&[0, 0, 0, 1, 1]).unwrap();
        }
        if close {
            drop(client);
        }
        let mut delivery = Delivery::frames(server, vec![b"not sent".to_vec()]).unwrap();
        assert!(
            delivery
                .poll(Instant::now() + Duration::from_secs(60))
                .is_err()
        );
        assert!(
            delivery
                .poll(Instant::now() + Duration::from_secs(60))
                .is_err()
        );
    }
}

#[test]
fn exact_fragmented_ack_requires_the_original_peer_eof() {
    let (mut client, server) = CtlStream::pair().unwrap();
    let mut delivery = Delivery::frames(server, vec![b"actual frame".to_vec()]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    assert!(!delivery.poll(deadline).unwrap());
    let mut bytes = Vec::new();
    client.read_to_end(&mut bytes).unwrap();
    for part in [&[0, 0][..], &[0, 1][..], &[1][..]] {
        client.write_all(part).unwrap();
        assert!(!delivery.poll(deadline).unwrap());
    }
    assert!(!delivery.control_closed(deadline).unwrap());
    // Publish the original peer's write EOF synchronously. Dropping its
    // socket alone may leave nonblocking observation briefly WouldBlock.
    client.shutdown(Shutdown::Write).unwrap();
    drop(client);
    assert!(delivery.poll(deadline).unwrap());
    assert!(delivery.control_closed(deadline).unwrap());
}

#[test]
fn queued_fragmented_ack_and_eof_are_observed_before_the_next_socket_message() {
    let (mut client, server) = CtlStream::pair().unwrap();
    let (mut next_sender, mut next_receiver) = CtlStream::pair().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut delivery = Delivery::frames(server, vec![b"previous query data".to_vec()]).unwrap();
    assert!(!delivery.poll(deadline).unwrap());
    let mut previous = Vec::new();
    client.read_to_end(&mut previous).unwrap();
    for byte in [0, 0, 0, 1, 1] {
        client.write_all(&[byte]).unwrap();
    }
    client.shutdown(Shutdown::Write).unwrap();
    // The client finishes its original ACK/write EOF before sending a new
    // message on another accepted connection. No timer/tick intervenes.
    next_sender.write_all(b"next-root-message").unwrap();
    let mut next = [0; 17];
    next_receiver.read_exact(&mut next).unwrap();
    assert_eq!(&next, b"next-root-message");
    assert!(
        !delivery.observation().acknowledged,
        "receiving the new message did not itself poll the prior socket"
    );

    let mut reads = 0;
    let mut interrupted = 0;
    assert!(
        delivery
            .receive_ack(deadline, |mut stream, bytes| {
                reads += 1;
                if reads == 1 || reads == 3 {
                    interrupted += 1;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                // Actual native one-byte reads deterministically exercise legal
                // short reads. No synthesized bytes/EOF or completion owner.
                stream.read(&mut bytes[..1])
            })
            .unwrap()
    );
    assert_eq!((reads, interrupted), (7, 2));
    let observed = delivery.observation();
    assert!(observed.write_closed && observed.acknowledged && !observed.failed);
    assert_eq!(observed.acknowledgement_prefix, [0, 0, 0, 1, 1]);
    assert_eq!(
        (observed.remaining_frames, observed.current_frame_sent),
        (0, 0)
    );
    assert!(delivery.poll(deadline).unwrap());
    // This checks transport causality only. Actual Owner transitions still
    // require consuming the same query's native/journal/loan closure owner.
}

#[test]
fn already_queued_partial_ack_eof_is_permanent_failure_with_exact_prefix() {
    let (mut client, server) = CtlStream::pair().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut delivery = Delivery::frames(server, vec![b"actual query frames".to_vec()]).unwrap();
    assert!(!delivery.poll(deadline).unwrap());
    client.read_to_end(&mut Vec::new()).unwrap();
    client.write_all(&[0, 0]).unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    assert!(delivery.poll(deadline).is_err());
    let observed = delivery.observation();
    assert!(observed.write_closed && observed.failed && !observed.acknowledged);
    assert_eq!(observed.acknowledgement_prefix, [0, 0]);
    assert!(delivery.poll(deadline).is_err());
}

#[test]
fn expired_original_ack_deadline_does_not_attempt_a_read_or_renew() {
    let (mut client, server) = CtlStream::pair().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut delivery = Delivery::frames(server, vec![b"actual query frames".to_vec()]).unwrap();
    assert!(!delivery.poll(deadline).unwrap());
    client.read_to_end(&mut Vec::new()).unwrap();
    client.write_all(&[0, 0, 0, 1, 1]).unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let expired = Instant::now();
    let mut attempted = false;
    let error = delivery
        .receive_ack(expired, |_, _| {
            attempted = true;
            Err(io::ErrorKind::Interrupted.into())
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(!attempted);
    assert!(delivery.observation().acknowledgement_prefix.is_empty());
}
