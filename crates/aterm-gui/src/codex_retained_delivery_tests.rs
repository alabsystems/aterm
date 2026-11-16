// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Real socket custody/timing only. These tests construct no SDK or foreground
//! grants; the service still checks those actual owners around the exchange.
use super::*;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

fn listener() -> (
    aterm_tempfile::TempDir,
    std::path::PathBuf,
    std::os::unix::net::UnixListener,
) {
    let directory = aterm_tempfile::tempdir().unwrap();
    let path = directory.path().join("frontend.sock");
    let listener = aterm_uds::kept_out(|| std::os::unix::net::UnixListener::bind(&path)).unwrap();
    (directory, path, listener)
}

fn accept(listener: &std::os::unix::net::UnixListener) -> CtlStream {
    let (stream, _) = aterm_uds::accept(
        listener,
        aterm_uds::AcceptWait::Until(Instant::now() + Duration::from_secs(60)),
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    assert_eq!(
        aterm_uds::fdpass::peer_pid(&stream).unwrap(),
        std::process::id()
    );
    stream
}

fn offer(stream: &CtlStream) -> ([u8; HEADER_BYTES], CtlStream) {
    let mut header = [0; HEADER_BYTES];
    let first = aterm_uds::fdpass::recv_with_fds(stream, &mut header, 1).unwrap();
    assert_eq!(first.fds.len(), 1);
    assert!(first.bytes > 0);
    let mut reader = stream;
    reader.read_exact(&mut header[first.bytes..]).unwrap();
    assert_eq!(&header[..2], b"F1");
    assert!(
        header[2..]
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    );
    let endpoint = CtlStream::from(first.fds.into_iter().next().unwrap());
    // SAFETY: F_GETFD only observes this live owned descriptor and takes no pointer.
    let flags = unsafe { libc::fcntl(endpoint.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    (header, endpoint)
}

#[test]
fn sent_callback_stays_connected_until_delayed_accept_peer_check_and_receipt() {
    let (_directory, path, listener) = listener();
    let (endpoint, mut backend) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    let (sent, offered) = mpsc::channel();
    let native = std::thread::spawn(move || {
        let owner = aterm_agent::harness::native_act::Process::observe(std::process::id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let callback = super::super::connect_callback(&path, &owner, deadline).unwrap();
        let mut delivery = FrontendDelivery::new(callback, endpoint).unwrap();
        delivery.offer(deadline, &mut || Ok(())).unwrap();
        sent.send(()).unwrap();
        delivery.receipt(deadline, &mut || Ok(())).unwrap();
        assert_eq!(delivery.phase, Phase::Received);
    });
    // The native send has completed before the caller accepts. Closing the
    // callback at send would make this later Darwin peer check fail ENOTCONN.
    offered.recv_timeout(Duration::from_secs(60)).unwrap();
    let mut callback = accept(&listener);
    let (header, mut received) = offer(&callback);
    callback.write_all(&header[2..9]).unwrap();
    callback.write_all(&header[9..]).unwrap();
    callback.shutdown(std::net::Shutdown::Write).unwrap();
    native.join().unwrap();
    received.write_all(b"actual endpoint").unwrap();
    let mut bytes = [0; 15];
    backend.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"actual endpoint");
}

fn uncertain_reply(
    reply: impl FnOnce(&mut CtlStream, &[u8; HEADER_BYTES], &CtlStream),
    receipt_budget: Duration,
) {
    let (_directory, path, listener) = listener();
    let (endpoint, _backend) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    let (sent, offered) = mpsc::channel();
    let native = std::thread::spawn(move || {
        let owner = aterm_agent::harness::native_act::Process::observe(std::process::id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let callback = super::super::connect_callback(&path, &owner, deadline).unwrap();
        let mut delivery = FrontendDelivery::new(callback, endpoint).unwrap();
        delivery.offer(deadline, &mut || Ok(())).unwrap();
        sent.send(()).unwrap();
        let result = delivery.receipt(Instant::now() + receipt_budget, &mut || Ok(()));
        (delivery, result)
    });
    offered.recv_timeout(Duration::from_secs(60)).unwrap();
    let mut callback = accept(&listener);
    let (header, received) = offer(&callback);
    reply(&mut callback, &header, &received);
    let (mut delivery, result) = native.join().unwrap();
    assert!(result.is_err());
    assert_eq!(delivery.phase, Phase::Uncertain);
    // SAFETY: F_GETFD only observes this live owned descriptor and takes no pointer.
    let flags = unsafe { libc::fcntl(delivery.callback.as_raw_fd(), libc::F_GETFD) };
    assert!(
        flags >= 0,
        "original callback remains owned after uncertainty"
    );
    // SAFETY: F_GETFD only observes this live owned descriptor and takes no pointer.
    let flags = unsafe { libc::fcntl(delivery.endpoint.as_raw_fd(), libc::F_GETFD) };
    assert!(
        flags >= 0,
        "original endpoint remains owned after uncertainty"
    );
    assert!(
        delivery
            .exchange(Instant::now() + Duration::from_secs(60), || Ok(()))
            .is_err()
    );
    callback.set_nonblocking(true).unwrap();
    let mut byte = [0];
    assert_eq!(
        aterm_uds::fdpass::recv_with_fds(&callback, &mut byte, 1)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock,
        "uncertain delivery did not resend a byte or right"
    );
}

#[test]
fn wrong_receipt_retains_original_delivery_without_resending() {
    uncertain_reply(
        |callback, _, _| {
            callback.write_all(&[b'x'; RECEIPT_BYTES]).unwrap();
            callback.shutdown(std::net::Shutdown::Write).unwrap();
        },
        Duration::from_secs(60),
    );
}

#[test]
fn eof_receipt_retains_original_delivery_without_resending() {
    uncertain_reply(
        |callback, _, _| {
            callback.shutdown(std::net::Shutdown::Write).unwrap();
        },
        Duration::from_secs(60),
    );
}

#[test]
fn lost_receipt_retains_original_delivery_without_resending() {
    uncertain_reply(|_, _, _| {}, Duration::from_millis(200));
}

#[test]
fn matching_receipt_with_extra_byte_is_uncertain_without_resending() {
    uncertain_reply(
        |callback, header, _| {
            callback.write_all(&header[2..]).unwrap();
            callback.write_all(b"x").unwrap();
            callback.shutdown(std::net::Shutdown::Write).unwrap();
        },
        Duration::from_secs(60),
    );
}

#[test]
fn receipt_with_a_right_is_uncertain_without_resending() {
    uncertain_reply(
        |callback, header, endpoint| {
            assert_eq!(
                aterm_uds::fdpass::send_with_fds(callback, &header[2..], &[endpoint.as_fd()])
                    .unwrap(),
                RECEIPT_BYTES
            );
            callback.shutdown(std::net::Shutdown::Write).unwrap();
        },
        Duration::from_secs(60),
    );
}

#[test]
fn matching_receipt_without_write_boundary_is_uncertain_without_resending() {
    uncertain_reply(
        |callback, header, _| {
            callback.write_all(&header[2..]).unwrap();
        },
        Duration::from_millis(200),
    );
}

#[test]
fn matching_receipt_cannot_replace_the_final_admission_check() {
    let (_directory, path, listener) = listener();
    let (endpoint, _backend) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    let admitted = Arc::new(AtomicBool::new(true));
    let current = admitted.clone();
    let (sent, offered) = mpsc::channel();
    let (waiting, entered) = mpsc::channel();
    let native = std::thread::spawn(move || {
        let owner = aterm_agent::harness::native_act::Process::observe(std::process::id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let callback = super::super::connect_callback(&path, &owner, deadline).unwrap();
        let mut delivery = FrontendDelivery::new(callback, endpoint).unwrap();
        delivery.offer(deadline, &mut || Ok(())).unwrap();
        sent.send(()).unwrap();
        let mut waiting = Some(waiting);
        assert!(
            delivery
                .receipt(deadline, &mut || {
                    if let Some(waiting) = waiting.take() {
                        waiting.send(()).unwrap();
                    }
                    if current.load(Ordering::Acquire) {
                        Ok(())
                    } else {
                        Err(Error::Stale)
                    }
                })
                .is_err()
        );
        assert_eq!(delivery.phase, Phase::Uncertain);
        delivery
    });
    offered.recv_timeout(Duration::from_secs(60)).unwrap();
    let mut callback = accept(&listener);
    let (header, _received) = offer(&callback);
    entered.recv_timeout(Duration::from_secs(60)).unwrap();
    admitted.store(false, Ordering::Release);
    callback.write_all(&header[2..]).unwrap();
    callback.shutdown(std::net::Shutdown::Write).unwrap();
    let _retained = native.join().unwrap();
}
