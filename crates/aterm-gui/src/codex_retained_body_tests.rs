// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Original socket/framing only. These tests issue no Native/SDK/UI permission.
use super::*;
use std::io::Read;
use std::time::Duration;

fn read_offer(socket: &mut CtlStream, tag: &[u8; 2], operation: &str, expected: &[u8]) -> [u8; 32] {
    socket
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut header = [0; 74];
    let first = aterm_uds::fdpass::recv_with_fds(socket, &mut header, 0).unwrap();
    assert!(first.fds.is_empty());
    assert!(first.bytes > 0);
    socket.read_exact(&mut header[first.bytes..]).unwrap();
    assert_eq!(&header[..2], tag);
    assert_eq!(&header[2..34], operation.as_bytes());
    assert_eq!(
        u64::from_be_bytes(header[66..].try_into().unwrap()),
        expected.len() as u64
    );
    let nonce: [u8; 32] = header[34..66].try_into().unwrap();
    assert!(
        nonce
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    );
    let mut body = vec![0; expected.len()];
    socket.read_exact(&mut body).unwrap();
    assert_eq!(body, expected);
    // This is BEFORE Source ACK. Omitting Native shutdown makes the actual
    // paired decoder wait rather than accepting a prefix or guessing an end.
    assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    nonce
}

#[test]
fn original_g1_and_e1_finish_write_before_receipt_and_never_replay() {
    for (tag, word) in [(Tag::Command, b"G1"), (Tag::Entry, b"E1")] {
        let (native, mut source) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        let operation = "a".repeat(32);
        let native_operation = operation.clone();
        let worker = std::thread::spawn(move || {
            let mut delivery =
                BodyDelivery::new(native, tag, &native_operation, b"original\0bytes").unwrap();
            delivery
                .exchange(Instant::now() + Duration::from_secs(60), || Ok(()))
                .unwrap();
            assert!(delivery.received());
            delivery
        });
        let nonce = read_offer(&mut source, word, &operation, b"original\0bytes");
        source.write_all(&nonce[..7]).unwrap();
        source.write_all(&nonce[7..]).unwrap();
        source.shutdown(std::net::Shutdown::Write).unwrap();
        let mut original = worker.join().unwrap();
        let mut rechecked = false;
        original
            .exchange(Instant::now() + Duration::from_secs(60), || {
                rechecked = true;
                Ok(())
            })
            .unwrap();
        assert!(
            rechecked,
            "a historical receipt still requires current admission"
        );
        assert_eq!(source.read(&mut [0]).unwrap(), 0);
    }
}

#[test]
fn extra_receipt_byte_or_right_cannot_complete_or_reoffer_the_original_body() {
    use std::os::fd::AsFd;
    for right in [false, true] {
        let (native, mut source) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        let operation = "b".repeat(32);
        let native_operation = operation.clone();
        let worker = std::thread::spawn(move || {
            let mut delivery =
                BodyDelivery::new(native, Tag::Entry, &native_operation, b"held input").unwrap();
            assert!(
                delivery
                    .exchange(Instant::now() + Duration::from_secs(60), || Ok(()))
                    .is_err()
            );
            assert!(!delivery.received());
            assert!(
                delivery
                    .exchange(Instant::now() + Duration::from_secs(60), || Ok(()))
                    .is_err()
            );
            delivery
        });
        let nonce = read_offer(&mut source, b"E1", &operation, b"held input");
        if right {
            let (reader, _writer) = aterm_uds::pipe().unwrap();
            assert_eq!(
                aterm_uds::fdpass::send_with_fds(&source, &nonce, &[reader.as_fd()]).unwrap(),
                nonce.len()
            );
        } else {
            source.write_all(&nonce).unwrap();
            source.write_all(b"x").unwrap();
        }
        source.shutdown(std::net::Shutdown::Write).unwrap();
        let _retained_original = worker.join().unwrap();
        assert_eq!(source.read(&mut [0]).unwrap(), 0);
    }
}
